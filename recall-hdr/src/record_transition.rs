//! The controller's side of the transition rules (`record_target_gate`):
//! what the tracker is told about a foreground window, the closed game's
//! drain, and the island notice when a retarget cuts that drain short.

use std::time::Instant;

use crate::log;
use crate::record_app::Controller;
use crate::record_engine::EngineTarget;
use crate::record_target::Target;
use crate::record_target_gate::{adopted_candidate, drain_cut, too_small, traits_of, Traits};
use crate::watcher::{self, WatchEvent};

impl Controller {
    /// The tracker's view of a foreground window: a new window of a process
    /// whose session window was just destroyed is adopted (T5a); otherwise
    /// the render check's classification, minus a windowed Steam-library
    /// window under the minimum size (T10).
    pub(crate) fn candidate(&mut self, ev: &WatchEvent, now: Instant) -> (Option<Target>, Traits) {
        if self.tracker.adopts(ev.pid, now) {
            if let Some(t) = adopted_candidate(ev, &self.cfg) {
                return (Some(t), Traits::ADOPTED);
            }
        }
        let Some(t) = self.classify(ev, now) else { return (None, Traits::FULLSCREEN) };
        let traits = traits_of(ev, &self.cfg);
        if too_small(traits, watcher::client_size(ev.hwnd)) {
            return (None, traits);
        }
        (Some(t), traits)
    }

    /// The closed game's drain while its last clip is still saveable: from
    /// the tick its session ended (before `plan_engine` starts the clock)
    /// until the drain runs out.
    pub(crate) fn live_drain(&self, now: Instant) -> Option<Target> {
        match self.planner.drain() {
            Some((t, at, len)) => (now < at + len).then(|| t.clone()),
            None => match self.planner.applied() {
                EngineTarget::Window(t) if self.tracker.current().is_none() && self.pinned.is_none() && !self.paused => Some(t.clone()),
                _ => None,
            },
        }
    }

    /// Before the tracker polls: tell it whose drain runs (T2).
    pub(crate) fn feed_drain(&mut self, now: Instant) {
        let pid = self.live_drain(now).map(|t| t.pid);
        self.tracker.set_draining(pid);
    }

    /// A retarget that ended another game's drain early: say so on the island.
    pub(crate) fn note_drain_cut(&self, drained: Option<&Target>, applied: &EngineTarget) {
        let (Some(a), Some(started)) = (drained, drain_cut(drained, applied)) else { return };
        log!("record: {:?}'s last clip dropped: {started:?} started during its drain", a.name);
        self.shortcut_notice("Last clip dropped", &format!("{started} started"));
    }
}

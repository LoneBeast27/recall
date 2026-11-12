//! When the UI pass (`root_poll`) runs next. It used to run every 30 ms in front and every
//! 250 ms behind, whatever happened. Now it runs when something it reads wakes it
//! (`services::ui_wake`), or at the next real deadline:
//!
//! Woken by the source (the source calls `ui_wake::wake` after it publishes):
//! - worker results (`Workers::done`, a `WakeSender`), clip folder and cache changes routed there;
//! - a recorder line that changed the snapshot, a status after a silence, the pipe thread's
//!   recorder-process check (`services::daemon`);
//! - an update check or Setup start finishing (`update`), a reinstall outcome (`repair`);
//! - a failed background library save (`ordered_write`);
//! - new meter levels and MIDI activity (the Audio page redraws on them, `services::meters`,
//!   `services::midi`);
//! - the window becoming active or inactive (a status drawn late, `back_in_front`);
//! - every frame drawn outside the editor: what the frame measured (the grid's width, the scroll
//!   position, a rename or cache-size field off screen) is read by the next pass.
//!
//! Deadlines ([`Root::deadlines`]): the toast running out, the storage-limit dry run (15 s), the
//! recorder's first start (1.5 s), the library's debounced save, local midnight ("Today"
//! becomes "Yesterday"), the SSD meter's next walk, the automatic update check, the recorder
//! counting as not responding, a drain countdown second, a starting recorder turning
//! unreachable, a pending fader gain running out.
//!
//! The pass keeps its old cadence (`timer-ok`) only while the editor shows or still works with
//! the editor closed (playback, loading, autosave, pre-encode: their player and copy state is
//! read, not signalled, and playback is left as it is), and for scripted checks
//! (`RECALL_NATIVE_TRACE` / `_LAYOUT`) that read every pass.
use std::time::{Duration, Instant};

use super::{Root, Route};

/// The old cadence, kept where the pass must read state that raises no event.
pub(super) const FRONT: Duration = Duration::from_millis(30);
pub(super) const BACK: Duration = Duration::from_millis(250);
/// A drain countdown is drawn per second; this is often enough to land on each.
const DRAIN_STEP: Duration = Duration::from_millis(250);
/// Shortest sleep: a deadline the pass failed to clear can never spin the UI thread.
const MIN_SLEEP: Duration = Duration::from_millis(15);

impl Root {
    /// How long the pass may sleep: `None` = until woken.
    pub(super) fn next_pass(&self) -> Option<Duration> {
        if self.needs_cadence() {
            // timer-ok: the editor's player, copy and pre-encode state is read every pass (playback
            // is out of scope), and scripted checks read every pass.
            return Some(if self.in_background() { BACK } else { FRONT });
        }
        let now = Instant::now();
        crate::services::ui_wake::earliest(now, self.deadlines(now)).map(|wait| wait.max(MIN_SLEEP))
    }

    /// The editor shows or still works, or a scripted check reads every pass.
    pub(super) fn needs_cadence(&self) -> bool {
        self.route == Route::Editor || self.ed.needs_ticks() || crate::trace::enabled()
    }

    /// Every time the pass must run with no wake: see the module comment.
    fn deadlines(&self, now: Instant) -> Vec<Option<Instant>> {
        use crate::views::storage_limit::ASK_TIMEOUT;
        let draining = self.snap.session.as_ref().is_some_and(|s| s.state == "draining");
        let pending_gain = self.settings_ui.audio.pending_gain.as_ref().filter(|_| self.route == Route::Audio);
        let mut at = vec![
            self.toast.as_ref().map(|t| t.until),
            // `poll_timers` gives up once the wait is over ASK_TIMEOUT.
            self.storage_asking.map(|(_, asked)| asked + ASK_TIMEOUT + Duration::from_millis(1)),
            self.controller_check_at,
            self.update.next_check(),
            // Not responding is drawn once, then the next status (a wake) clears it.
            self.daemon.unresponsive_at().filter(|_| !self.recorder_unresponsive),
            draining.then(|| now + DRAIN_STEP),
            self.reach_deadline(),
            pending_gain.map(|(_, _, set)| *set + crate::services::audio_sources::PENDING_GAIN_FOR),
            self.lib.ssd.next_walk(!self.lib.loading),
            // The on-screen thumbnail check's backstop (`queue_visible`).
            self.lib.thumb_backstop().filter(|_| self.route == Route::Library),
            // A removed source's Undo ends even when another toast replaced its own.
            self.settings_ui.audio.removed.as_ref().map(|(_, _, at)| *at + crate::views::audio_page::UNDO_FOR),
        ];
        at.extend(self.lib.upkeep_deadlines());
        at
    }

    /// The Audio page's meters or MIDI log moved since the last pass, a fader's pending gain
    /// ran out (the strip shows the saved gain again), or a removed source's Undo ran out. True
    /// when it should draw. Meter levels wake the pass only while Recall is in front.
    pub(super) fn audio_moved(&mut self) -> bool {
        let undo_over = self.settings_ui.audio.removed.as_ref().is_some_and(|(_, _, at)| at.elapsed() >= crate::views::audio_page::UNDO_FOR);
        if undo_over {
            self.settings_ui.audio.removed = None;
        }
        crate::services::meters::set_wakes(self.route == Route::Audio && !self.in_background());
        if self.route != Route::Audio {
            return undo_over;
        }
        let meters = self.meters.as_ref().map_or(0, |m| m.revision());
        let midi = self.midi.as_ref().map_or(0, |m| m.revision());
        let mut moved = (meters, midi) != self.audio_seen;
        self.audio_seen = (meters, midi);
        let pending = &mut self.settings_ui.audio.pending_gain;
        // `strip_gain` stops showing it at the same age; forgetting it here clears the deadline.
        if pending.as_ref().is_some_and(|(_, _, set)| set.elapsed() >= crate::services::audio_sources::PENDING_GAIN_FOR) {
            *pending = None;
            moved = true;
        }
        moved || undo_over
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_old_poll_duty_has_a_wake_or_a_deadline() {
        // The pass (`root_poll::poll`) and what now runs it: a source wake or a deadline here.
        let poll = include_str!("root_poll.rs");
        let wake = include_str!("root_wake.rs");
        for duty in ["self.workers.done.try_recv()", "self.poll_daemon()", "self.recorder_clock_tick()", "self.reach_tick()", "self.lib.roll_day()",
            "self.lib.flush_meta_if_due()", "self.lib.ssd.poll(", "self.poll_timers()", "self.update.tick()", "crate::repair::take_outcome()", "self.audio_moved()"] {
            assert!(poll.contains(duty), "the pass still runs {duty}");
        }
        for deadline in ["t.until", "ASK_TIMEOUT", "self.controller_check_at", "next_check()", "unresponsive_at()", "DRAIN_STEP", "reach_deadline()",
            "PENDING_GAIN_FOR", "next_walk(", "upkeep_deadlines()"] {
            assert!(wake.contains(deadline), "a deadline wakes the pass for {deadline}");
        }
        let sources = [
            include_str!("services/daemon.rs"),
            include_str!("services/update.rs"),
            include_str!("services/repair.rs"),
            include_str!("services/ordered_write.rs"),
            include_str!("services/meters.rs"),
            include_str!("services/midi.rs"),
        ];
        for source in sources {
            assert!(source.contains("ui_wake::wake()"), "each source wakes the pass");
        }
        assert!(include_str!("worker.rs").contains("pub type DoneSender = crate::services::ui_wake::WakeSender<Done>;"), "worker results wake it");
        assert!(include_str!("root_render.rs").contains("ui_wake::wake()"), "a drawn frame wakes it");
        assert!(poll.contains("observe_window_activation"), "activation wakes it");
    }
}

//! What the engine is busy with, from the controller's side: replay saves in
//! flight and the manual long recording. Settings changes and retargets that
//! need an engine restart wait until neither is running (`pending_restart` in
//! the session status), and a pause first stops the long recording
//! ([`PendingPause`]); engine events arrive through `Supervisor::attach_events`.
//! A retarget (game closed past its drain, or a new game) stops the long
//! recording itself ([`AutoStop`]) instead of waiting on it.
//!
//! Every timer here runs on the awake-only clock (`awake_clock`, design-detect-release §0):
//! a sleep ages no save window, cap or wait.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::awake_clock::Awake;
use crate::record_engine::Busy;

/// Saves in flight with no `saved` / `save_failed` this long after the queue
/// started, the engine last answered one, or the save being written last grew
/// (`status.save_writing`, [`writing`]) are assumed lost (one window for the
/// queue while the engine keeps answering or writing, not one per save).
pub const SAVE_STALE: Duration = Duration::from_secs(120);
/// A `record_start` with no `recording_started` / `_failed` after this is dropped
/// (the engine's own keyframe wait is 30 s).
pub const START_STALE: Duration = Duration::from_secs(35);
/// Why a long recording cannot start while a closed game drains.
pub const DRAIN_RECORD_REASON: &str = "The game closed; nothing to record";
/// How long a pause waits for the long recording to report stopping.
pub const PAUSE_STOP_WAIT: Duration = Duration::from_secs(10);
/// The island while a pause waits for the save being written (R5-overlay-1).
pub const PAUSE_AFTER_SAVE: &str = "Pausing after the save";
pub const PAUSE_AFTER_SAVE_DETAIL: &str = "Recording stops once the clip is saved";
/// The island once a restart has waited 30 s for a save (`writing::HOLD_NOTICE_AFTER`).
pub const SAVE_HOLD_TITLE: &str = "Waiting for the clip to finish saving";
pub const SAVE_HOLD_DETAIL: &str = "The recorder restarts once it is saved";
/// Why a screenshot is refused while a closed game drains (it would be the
/// closed game's last frame).
pub const DRAIN_SCREENSHOT_REASON: &str = "The game closed";
/// Screenshots with no `screenshot` / `screenshot_failed` this long after the
/// last one was sent are assumed lost (a PNG can go to a sleeping hard drive).
pub const SCREENSHOT_STALE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub enum LongRec {
    Idle,
    Starting { since: Awake },
    /// `started_unix`: Unix seconds when the engine reported it running; `since`:
    /// the same moment on the monotonic clock (the status's `elapsed_secs`).
    Active { path: String, started_unix: f64, since: Instant },
}

/// The long recording the controller stopped for a retarget (its
/// `record_stop` is sent once): the retarget goes ahead when the engine
/// reports it ended, or after [`PAUSE_STOP_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoStop {
    deadline: Awake,
    /// The game closed (else a new game is wanted).
    game_closed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineActivity {
    pending_saves: u32,
    /// When the queue of saves started, or the engine last answered one of it
    /// (or reported its bytes growing).
    save_progress: Option<Awake>,
    /// Saves of a queue that expired unanswered: their late answers land here.
    expired_saves: u32,
    /// The engine's own report of the save it is writing.
    write: writing::WriteWatch,
    /// Screenshots sent and not yet reported (their PNGs are still being written).
    pending_screenshots: u32,
    last_screenshot_sent: Option<Awake>,
    pub long: LongRec,
    auto_stop: Option<AutoStop>,
    /// A `record_stop` (any sender: the user, a pause, a retarget, sleep, a session end) went
    /// to the engine and its `recording_stopped` has not come yet.
    stop_requested: bool,
    /// The clip size a save is expected to reach: sets how long a growing save
    /// may hold restarts (`writing::hold_cap`). Kept across engine instances.
    save_estimate: u64,
}

impl Default for EngineActivity {
    fn default() -> Self {
        EngineActivity {
            pending_saves: 0,
            save_progress: None,
            expired_saves: 0,
            write: writing::WriteWatch::default(),
            pending_screenshots: 0,
            last_screenshot_sent: None,
            long: LongRec::Idle,
            auto_stop: None,
            stop_requested: false,
            save_estimate: 0,
        }
    }
}

/// An engine event the controller acts on beyond bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// A clip landed: run the storage cap.
    Saved,
    /// Long recording started / stopped / failed: publish status.
    LongRecChanged,
    /// The long recording a retarget stopped ([`AutoStop`]) was saved.
    AutoStopped { game_closed: bool },
    /// The engine stopped with this many saves in flight: they are lost.
    SavesLost(u32),
}

/// Why a save in flight failed when the engine stopped.
pub const LOST_SAVE_REASON: &str = "The recorder stopped during the save";

/// The `save_failed` each lost save gets (mod `engine`, as the engine's own),
/// so the island's ring ends in "Save failed" instead of silence.
pub fn lost_save_event() -> String {
    json!({"type": "save_failed", "reason": LOST_SAVE_REASON}).to_string()
}

impl EngineActivity {
    /// A save went to the engine. A save queued behind others waits in their
    /// window: only a new queue (or an answer) starts one.
    pub fn save_sent(&mut self, now: Awake) {
        self.expire_saves(now);
        if self.pending_saves == 0 {
            self.save_progress = Some(now);
        }
        self.pending_saves += 1;
    }

    /// Saves whose window ran out are lost: an engine that keeps answering
    /// never gets here while saves still write.
    fn expire_saves(&mut self, now: Awake) {
        if self.save_progress.is_some_and(|t| now.duration_since(t) >= SAVE_STALE) {
            self.expired_saves += std::mem::take(&mut self.pending_saves);
        }
    }

    /// `cmd` (`engine_command`) went to the engine: a long-recording start or
    /// a screenshot is now in flight.
    pub fn command_sent(&mut self, cmd: &str, now: Awake) {
        let starts = cmd == "record_start" || (cmd == "record_toggle" && !self.is_recording());
        if cmd == "record_stop" || (cmd == "record_toggle" && self.is_recording()) {
            self.stop_sent();
        } else if starts {
            self.record_requested(now);
        }
        if cmd == "screenshot" {
            self.pending_screenshots += 1;
            self.last_screenshot_sent = Some(now);
        }
    }

    /// A `record_start` / `record_toggle` went to an idle engine.
    pub fn record_requested(&mut self, now: Awake) {
        if self.long == LongRec::Idle {
            self.long = LongRec::Starting { since: now };
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.long, LongRec::Active { .. })
    }

    /// A `record_stop` went to the engine (the caller checked it was queued). With no long
    /// recording there is nothing to wait for.
    pub fn stop_sent(&mut self) {
        self.stop_requested = self.long != LongRec::Idle;
    }

    /// A stop is on its way: a session end must not restart this recording on a cancel.
    pub fn stop_in_flight(&self) -> bool {
        self.stop_requested
    }

    /// A long recording the controller asked to start has not reported running yet.
    pub fn is_starting(&self) -> bool {
        matches!(self.long, LongRec::Starting { .. })
    }

    /// A long recording running or still starting: a session end stops it (a start sent
    /// just before the query would otherwise run on unsaved) and a cancel restarts it.
    pub fn recording_or_starting(&self) -> bool {
        matches!(self.long, LongRec::Active { .. } | LongRec::Starting { .. })
    }

    /// A new engine instance: nothing of the old one is in flight any more.
    pub fn engine_restarted(&mut self) {
        let save_estimate = self.save_estimate;
        *self = EngineActivity { save_estimate, ..EngineActivity::default() };
    }

    /// Track one engine event; `unix_now` stamps a recording start.
    pub fn on_event(&mut self, ev: &Value, unix_now: f64) -> Option<Trigger> {
        let text = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        // A part of a recording whose capture size changed: it goes on in a new file.
        let continued = ev.get("continued").and_then(Value::as_bool).unwrap_or(false);
        match ev.get("type").and_then(Value::as_str)? {
            "saved" => {
                self.answered(ev);
                Some(Trigger::Saved)
            }
            "save_failed" => {
                self.answered(ev);
                None
            }
            // The supervisor's own event: the engine process ended. Its last
            // events came first, so whatever is still pending is lost.
            crate::supervisor::ENGINE_EXITED => {
                (self.expired_saves, self.write) = (0, writing::WriteWatch::default());
                let lost = std::mem::take(&mut self.pending_saves);
                (lost > 0).then_some(Trigger::SavesLost(lost))
            }
            "screenshot" | "screenshot_failed" => {
                self.pending_screenshots = self.pending_screenshots.saturating_sub(1);
                None
            }
            "recording_started" => {
                let (started_unix, since) = match self.long {
                    LongRec::Active { started_unix, since, .. } if continued => (started_unix, since),
                    _ => (unix_now, Instant::now()),
                };
                self.long = LongRec::Active { path: text("path"), started_unix, since };
                Some(Trigger::LongRecChanged)
            }
            // Only a part ended: the recording stays Active (no retarget slips in
            // between the parts), and an auto stop waits for the last part.
            "recording_stopped" if continued => Some(Trigger::LongRecChanged),
            kind @ ("recording_stopped" | "recording_failed") => {
                self.long = LongRec::Idle;
                self.stop_requested = false;
                match self.auto_stop.take() {
                    Some(stop) if kind == "recording_stopped" => Some(Trigger::AutoStopped { game_closed: stop.game_closed }),
                    _ => Some(Trigger::LongRecChanged),
                }
            }
            _ => None,
        }
    }

    /// One engine event as it arrives at `now` (the live path): a save's
    /// answer shows the engine alive, so the saves still queued get a new window,
    /// and so does a `status` whose `save_writing` bytes grew. A rejection only
    /// shows the main loop alive, not the writer: it moves no window.
    pub fn on_engine_event(&mut self, ev: &Value, unix_now: f64, now: Awake) -> Option<Trigger> {
        let kind = ev.get("type").and_then(Value::as_str);
        // A released engine's drain reports `save_writing` alone (`save_progress`).
        if matches!(kind, Some("status" | "save_progress")) {
            self.on_status(ev, now);
            return None;
        }
        let rejected = ev.get("rejected").and_then(Value::as_bool) == Some(true);
        let answer = matches!(kind, Some("saved" | "save_failed")) && !rejected;
        let trigger = self.on_event(ev, unix_now);
        if answer {
            self.front_answered(now);
        }
        if answer && self.pending_saves > 0 {
            self.save_progress = Some(now);
        }
        trigger
    }

    /// Saves or a screenshot in flight, or a long recording starting / running: an engine
    /// restart now would lose work.
    pub fn busy(&mut self, now: Awake) -> bool {
        self.expire_saves(now);
        if let LongRec::Starting { since } = self.long {
            if now.duration_since(since) >= START_STALE {
                self.long = LongRec::Idle;
                self.stop_requested = false;
            }
        }
        if self.last_screenshot_sent.is_some_and(|t| now.duration_since(t) >= SCREENSHOT_STALE) {
            self.pending_screenshots = 0;
        }
        self.saves_writing(now) || self.pending_screenshots > 0 || self.long != LongRec::Idle
    }

    /// What holds a due retarget now (see `EnginePlanner::step`): saves or a
    /// screenshot in flight, and an active long recording whose stop has not timed out. A
    /// recording still starting has nothing to save: the retarget's engine
    /// restart drops it.
    pub fn retarget_busy(&mut self, now: Awake) -> Busy {
        self.busy(now); // expire stale saves / starts
        let given_up = self.auto_stop.is_some_and(|s| now >= s.deadline);
        let saving = self.saves_writing(now) || self.pending_screenshots > 0;
        Busy { saving, long_rec: self.is_recording() && !given_up }
    }

    /// A retarget needs the active long recording stopped. `true` = send
    /// `record_stop` now; once per recording, later calls only wait.
    pub fn auto_stop(&mut self, now: Awake, game_closed: bool) -> bool {
        if !self.is_recording() || self.auto_stop.is_some() {
            return false;
        }
        self.auto_stop = Some(AutoStop { deadline: now + PAUSE_STOP_WAIT, game_closed });
        true
    }

    /// Why `cmd` (a long-recording command or a screenshot) is refused now: a
    /// start while a closed game drains has no window to record. `None` = send it.
    pub fn record_refusal(&self, cmd: &str, draining: bool) -> Option<&'static str> {
        if draining && cmd == "screenshot" {
            return Some(DRAIN_SCREENSHOT_REASON);
        }
        let starts = cmd == "record_start" || (cmd == "record_toggle" && !self.is_recording());
        (draining && starts).then_some(DRAIN_RECORD_REASON)
    }

    /// Session-status `long_recording`.
    pub fn long_recording_json(&self) -> Value {
        self.long_recording_json_at(Instant::now())
    }

    /// `long_recording` at `now`.
    pub fn long_recording_json_at(&self, now: Instant) -> Value {
        match &self.long {
            LongRec::Active { path, started_unix, since } => {
                let elapsed = now.saturating_duration_since(*since).as_millis() as f64 / 1000.0;
                json!({"active": true, "path": path, "started_secs": started_unix, "elapsed_secs": elapsed})
            }
            LongRec::Starting { .. } => json!({"active": false, "starting": true, "path": null, "started_secs": null}),
            LongRec::Idle => json!({"active": false, "path": null, "started_secs": null}),
        }
    }
}

/// A pause held back while the long recording stops (its `record_stop` is
/// sent) and while a save is still being written (the engine's quit would abort
/// it): it applies once the engine reports `recording_stopped` /
/// `recording_failed` (or after [`PAUSE_STOP_WAIT`]) and no save is written any
/// more (bounded by the save's own window and cap, `writing`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingPause {
    deadline: Awake,
}

impl PendingPause {
    /// `Some` when a pause asked at `now` must wait: the long recording (starting
    /// or running) is stopped first, a save being written finishes first.
    pub fn needed(activity: &mut EngineActivity, now: Awake) -> Option<PendingPause> {
        let wait = activity.long != LongRec::Idle || activity.saves_writing(now);
        wait.then(|| PendingPause { deadline: now + PAUSE_STOP_WAIT })
    }

    /// The recording reported its end (or its wait ran out), and no save is
    /// still being written.
    pub fn ready(&self, activity: &mut EngineActivity, now: Awake) -> bool {
        (activity.long == LongRec::Idle || now >= self.deadline) && !activity.saves_writing(now)
    }
}

#[cfg(test)]
#[path = "record_activity_tests.rs"]
mod tests;

#[path = "record_activity_writing.rs"]
mod writing;

#[cfg(test)]
#[path = "record_activity_queue_tests.rs"]
mod queue_tests;

#[cfg(test)]
#[path = "record_activity_session_tests.rs"]
mod session_tests;

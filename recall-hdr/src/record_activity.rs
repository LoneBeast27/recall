//! What the engine is busy with, from the controller's side: replay saves in
//! flight and the manual long recording. Settings changes and retargets that
//! need an engine restart wait until neither is running (`pending_restart` in
//! the session status), and a pause first stops the long recording
//! ([`PendingPause`]); engine events arrive through `Supervisor::attach_events`.
//! A retarget (game closed past its drain, or a new game) stops the long
//! recording itself ([`AutoStop`]) instead of waiting on it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::record_engine::Busy;

/// A save with no `saved` / `save_failed` after this is assumed lost.
pub const SAVE_STALE: Duration = Duration::from_secs(120);
/// A `record_start` with no `recording_started` / `_failed` after this is dropped
/// (the engine's own keyframe wait is 30 s).
pub const START_STALE: Duration = Duration::from_secs(35);
/// Why a long recording cannot start while a closed game drains.
pub const DRAIN_RECORD_REASON: &str = "The game closed; nothing to record";
/// How long a pause waits for the long recording to report stopping.
pub const PAUSE_STOP_WAIT: Duration = Duration::from_secs(10);
/// Why a screenshot is refused while a closed game drains (it would be the
/// closed game's last frame).
pub const DRAIN_SCREENSHOT_REASON: &str = "The game closed";
/// Screenshots with no `screenshot` / `screenshot_failed` this long after the
/// last one was sent are assumed lost (a PNG can go to a sleeping hard drive).
pub const SCREENSHOT_STALE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub enum LongRec {
    Idle,
    Starting { since: Instant },
    /// `started_unix`: Unix seconds when the engine reported it running.
    Active { path: String, started_unix: f64 },
}

/// The long recording the controller stopped for a retarget (its
/// `record_stop` is sent once): the retarget goes ahead when the engine
/// reports it ended, or after [`PAUSE_STOP_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoStop {
    deadline: Instant,
    /// The game closed (else a new game is wanted).
    game_closed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineActivity {
    pending_saves: u32,
    last_save_sent: Option<Instant>,
    /// Screenshots sent and not yet reported (their PNGs are still being written).
    pending_screenshots: u32,
    last_screenshot_sent: Option<Instant>,
    pub long: LongRec,
    auto_stop: Option<AutoStop>,
    /// A `record_stop` (any sender: the user, a pause, a retarget, sleep, a session end) went
    /// to the engine and its `recording_stopped` has not come yet.
    stop_requested: bool,
}

impl Default for EngineActivity {
    fn default() -> Self {
        EngineActivity {
            pending_saves: 0,
            last_save_sent: None,
            pending_screenshots: 0,
            last_screenshot_sent: None,
            long: LongRec::Idle,
            auto_stop: None,
            stop_requested: false,
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
}

impl EngineActivity {
    pub fn save_sent(&mut self, now: Instant) {
        self.pending_saves += 1;
        self.last_save_sent = Some(now);
    }

    /// `cmd` (`engine_command`) went to the engine: a long-recording start or
    /// a screenshot is now in flight.
    pub fn command_sent(&mut self, cmd: &str, now: Instant) {
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
    pub fn record_requested(&mut self, now: Instant) {
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
        *self = EngineActivity::default();
    }

    /// Track one engine event; `unix_now` stamps a recording start.
    pub fn on_event(&mut self, ev: &Value, unix_now: f64) -> Option<Trigger> {
        let text = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        // A part of a recording whose capture size changed: it goes on in a new file.
        let continued = ev.get("continued").and_then(Value::as_bool).unwrap_or(false);
        match ev.get("type").and_then(Value::as_str)? {
            "saved" => {
                self.pending_saves = self.pending_saves.saturating_sub(1);
                Some(Trigger::Saved)
            }
            "save_failed" => {
                self.pending_saves = self.pending_saves.saturating_sub(1);
                None
            }
            "screenshot" | "screenshot_failed" => {
                self.pending_screenshots = self.pending_screenshots.saturating_sub(1);
                None
            }
            "recording_started" => {
                let started_unix = match self.long {
                    LongRec::Active { started_unix, .. } if continued => started_unix,
                    _ => unix_now,
                };
                self.long = LongRec::Active { path: text("path"), started_unix };
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

    /// Saves or a screenshot in flight, or a long recording starting / running: an engine
    /// restart now would lose work.
    pub fn busy(&mut self, now: Instant) -> bool {
        if self.last_save_sent.is_some_and(|t| now.duration_since(t) >= SAVE_STALE) {
            self.pending_saves = 0;
        }
        if let LongRec::Starting { since } = self.long {
            if now.duration_since(since) >= START_STALE {
                self.long = LongRec::Idle;
                self.stop_requested = false;
            }
        }
        if self.last_screenshot_sent.is_some_and(|t| now.duration_since(t) >= SCREENSHOT_STALE) {
            self.pending_screenshots = 0;
        }
        self.pending_saves > 0 || self.pending_screenshots > 0 || self.long != LongRec::Idle
    }

    /// What holds a due retarget now (see `EnginePlanner::step`): saves or a
    /// screenshot in flight, and an active long recording whose stop has not timed out. A
    /// recording still starting has nothing to save: the retarget's engine
    /// restart drops it.
    pub fn retarget_busy(&mut self, now: Instant) -> Busy {
        self.busy(now); // expire stale saves / starts
        let given_up = self.auto_stop.is_some_and(|s| now >= s.deadline);
        let saving = self.pending_saves > 0 || self.pending_screenshots > 0;
        Busy { saving, long_rec: self.is_recording() && !given_up }
    }

    /// A retarget needs the active long recording stopped. `true` = send
    /// `record_stop` now; once per recording, later calls only wait.
    pub fn auto_stop(&mut self, now: Instant, game_closed: bool) -> bool {
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
        match &self.long {
            LongRec::Active { path, started_unix } => json!({"active": true, "path": path, "started_secs": started_unix}),
            LongRec::Starting { .. } => json!({"active": false, "starting": true, "path": null, "started_secs": null}),
            LongRec::Idle => json!({"active": false, "path": null, "started_secs": null}),
        }
    }
}

/// A pause held back while the long recording stops (its `record_stop` is
/// sent): it applies once the engine reports `recording_stopped` /
/// `recording_failed`, or after [`PAUSE_STOP_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingPause {
    deadline: Instant,
}

impl PendingPause {
    /// `Some` when a pause asked at `now` must first stop the long recording
    /// (starting or running).
    pub fn needed(activity: &EngineActivity, now: Instant) -> Option<PendingPause> {
        (activity.long != LongRec::Idle).then(|| PendingPause { deadline: now + PAUSE_STOP_WAIT })
    }

    /// The recording reported its end, or the wait ran out.
    pub fn ready(&self, activity: &EngineActivity, now: Instant) -> bool {
        activity.long == LongRec::Idle || now >= self.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_waits_for_the_long_recording_to_stop_but_not_forever() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert_eq!(PendingPause::needed(&a, t0), None, "nothing recording: pause at once");
        a.record_requested(t0);
        assert!(PendingPause::needed(&a, t0).is_some(), "a starting recording is stopped first too");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        let wait = PendingPause::needed(&a, t0).expect("recording: stop first");
        assert!(!wait.ready(&a, t0 + Duration::from_secs(1)));
        a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":1.0,"bytes":1}), 0.0);
        assert!(wait.ready(&a, t0 + Duration::from_secs(1)), "stopped: pause now");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert!(!wait.ready(&a, t0 + PAUSE_STOP_WAIT - Duration::from_millis(1)));
        assert!(wait.ready(&a, t0 + PAUSE_STOP_WAIT), "an engine that never answers does not block the pause");
    }

    #[test]
    fn saves_in_flight_block_restarts_until_reported_or_stale() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert!(!a.busy(t0));
        a.save_sent(t0);
        a.save_sent(t0);
        assert!(a.busy(t0));
        assert_eq!(a.on_event(&json!({"type":"saved","path":"x.mp4"}), 0.0), Some(Trigger::Saved));
        assert!(a.busy(t0), "one more in flight");
        assert_eq!(a.on_event(&json!({"type":"save_failed","reason":"r"}), 0.0), None);
        assert!(!a.busy(t0));
        a.save_sent(t0);
        assert!(!a.busy(t0 + SAVE_STALE), "a lost save does not block forever");
    }

    #[test]
    fn long_recording_lifecycle_and_status_shape() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert_eq!(a.long_recording_json(), json!({"active":false,"path":null,"started_secs":null}));
        a.record_requested(t0);
        assert!(a.busy(t0) && !a.is_recording());
        a.on_event(&json!({"type":"recording_started","path":"E:\\c\\G__rec.mp4"}), 1_790_000_000.5);
        assert!(a.is_recording());
        assert_eq!(
            a.long_recording_json(),
            json!({"active":true,"path":"E:\\c\\G__rec.mp4","started_secs":1_790_000_000.5})
        );
        assert_eq!(a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":3.0,"bytes":9}), 0.0), Some(Trigger::LongRecChanged));
        assert!(!a.busy(t0));
        a.record_requested(t0);
        assert!(!a.busy(t0 + START_STALE), "a start that never answered expires");
        a.record_requested(t0);
        a.on_event(&json!({"type":"recording_failed","reason":"x","path":null}), 0.0);
        assert_eq!(a.long, LongRec::Idle);
    }

    #[test]
    fn a_retarget_stops_the_long_recording_once_and_waits_for_it_bounded() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut a = EngineActivity::default();
        assert!(!a.auto_stop(t0, true), "nothing recording: nothing to stop");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert_eq!(a.retarget_busy(t0), Busy { saving: false, long_rec: true });
        assert!(a.auto_stop(t0, true), "first ask sends record_stop");
        assert!(!a.auto_stop(t0 + s(1), true), "sent once per recording");
        assert!(a.retarget_busy(t0 + s(1)).long_rec, "waits for the engine to report it");
        let ev = a.on_event(&json!({"type":"recording_stopped","path":"p","seconds":9.0,"bytes":1}), 0.0);
        assert_eq!(ev, Some(Trigger::AutoStopped { game_closed: true }), "the island says why it stopped");
        assert_eq!(a.retarget_busy(t0 + s(1)), Busy::default(), "stopped: retarget now");
        // An engine that never answers holds the retarget only PAUSE_STOP_WAIT.
        a.on_event(&json!({"type":"recording_started","path":"q"}), 1.0);
        assert!(a.auto_stop(t0 + s(20), false));
        assert!(a.retarget_busy(t0 + s(20) + PAUSE_STOP_WAIT - Duration::from_millis(1)).long_rec);
        assert!(!a.retarget_busy(t0 + s(20) + PAUSE_STOP_WAIT).long_rec, "gave up on the stop");
        // A user's own stop (no auto stop) is the plain event.
        let mut b = EngineActivity::default();
        b.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert_eq!(b.on_event(&json!({"type":"recording_stopped","path":"p"}), 0.0), Some(Trigger::LongRecChanged));
        // A save in flight still holds, bounded by SAVE_STALE.
        b.save_sent(t0);
        assert_eq!(b.retarget_busy(t0 + s(1)), Busy { saving: true, long_rec: false });
        assert_eq!(b.retarget_busy(t0 + SAVE_STALE), Busy::default());
    }

    #[test]
    fn a_long_recording_cannot_start_while_a_closed_game_drains() {
        let mut a = EngineActivity::default();
        for cmd in ["record_start", "record_toggle"] {
            assert_eq!(a.record_refusal(cmd, true), Some(DRAIN_RECORD_REASON), "{cmd}");
            assert_eq!(a.record_refusal(cmd, false), None, "{cmd}: on target it starts");
        }
        assert_eq!(a.record_refusal("record_stop", true), None);
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert_eq!(a.record_refusal("record_toggle", true), None, "a toggle that stops is never refused");
        assert_eq!(a.record_refusal("record_start", true), Some(DRAIN_RECORD_REASON));
    }

    #[test]
    fn a_starting_long_recording_never_holds_a_retarget() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        a.record_requested(t0);
        assert_eq!(a.retarget_busy(t0 + Duration::from_secs(1)), Busy::default(), "not yet recording: nothing to save");
        assert!(!a.auto_stop(t0 + Duration::from_secs(1), true), "nothing to stop either");
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        assert!(a.retarget_busy(t0 + Duration::from_secs(2)).long_rec, "an active recording is stopped first");
    }

    #[test]
    fn a_game_recreating_its_window_during_a_long_recording_is_never_held_forever() {
        use crate::record_engine::{EnginePlanner, EngineTarget, Step, EXIT_LINGER};
        use crate::record_target::Target;
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let win = |hwnd| EngineTarget::Window(Target { hwnd, pid: 7, name: "G".into(), exe: "G.exe".into() });
        let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
        assert_eq!(p.step(&win(1), t0, a.retarget_busy(t0)), Step::Apply(win(1)));
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        // Fullscreen -> windowed: same pid, new hwnd. The engine never answers the stop.
        let mut sent = 0;
        let mut applied_at = None;
        for tick in 1..=3600u64 {
            let now = t0 + s(tick);
            match p.step(&win(2), now, a.retarget_busy(now)) {
                Step::StopLongRec { game_closed } => {
                    assert!(!game_closed);
                    sent += a.auto_stop(now, game_closed) as u32;
                }
                Step::Apply(t) => {
                    assert_eq!(t, win(2));
                    applied_at = Some(tick);
                    break;
                }
                other => panic!("tick {tick}: {other:?}"),
            }
        }
        assert_eq!(sent, 1, "record_stop sent once");
        assert_eq!(applied_at, Some(1 + PAUSE_STOP_WAIT.as_secs()), "retargeted once the stop timed out");
    }

    #[test]
    fn a_screenshot_is_refused_while_a_closed_game_drains() {
        let a = EngineActivity::default();
        assert_eq!(a.record_refusal("screenshot", true), Some(DRAIN_SCREENSHOT_REASON), "the closed game's last frame");
        assert_eq!(a.record_refusal("screenshot", false), None, "on target it is taken");
        assert_eq!(a.record_refusal("bookmark", true), None);
    }

    #[test]
    fn a_screenshot_in_flight_holds_a_retarget_until_reported_or_stale() {
        use crate::record_engine::{EnginePlanner, EngineTarget, Step, EXIT_LINGER};
        use crate::record_target::Target;
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let win = |pid, hwnd| EngineTarget::Window(Target { hwnd, pid, name: "G".into(), exe: "G.exe".into() });
        let (mut p, mut a) = (EnginePlanner::new(EXIT_LINGER), EngineActivity::default());
        assert_eq!(p.step(&win(7, 1), t0, a.retarget_busy(t0)), Step::Apply(win(7, 1)));
        a.command_sent("screenshot", t0);
        assert!(a.busy(t0), "a settings restart waits for the PNG");
        assert_eq!(p.step(&win(8, 2), t0 + s(1), a.retarget_busy(t0 + s(1))), Step::Held, "a new game waits for the PNG");
        a.on_event(&json!({"type":"screenshot","path":"G.png"}), 0.0);
        assert!(!a.busy(t0 + s(1)));
        assert_eq!(p.step(&win(8, 2), t0 + s(1), a.retarget_busy(t0 + s(1))), Step::Apply(win(8, 2)));
        a.command_sent("screenshot", t0 + s(2));
        a.on_event(&json!({"type":"screenshot_failed","reason":"r"}), 0.0);
        assert!(!a.busy(t0 + s(2)), "a failed screenshot is over too");
        a.command_sent("screenshot", t0 + s(3));
        assert!(a.busy(t0 + s(3) + SCREENSHOT_STALE - Duration::from_millis(1)));
        assert!(!a.busy(t0 + s(3) + SCREENSHOT_STALE), "a lost screenshot does not block forever");
        assert_eq!(a.retarget_busy(t0 + s(3) + SCREENSHOT_STALE), Busy::default());
    }

    #[test]
    fn screenshots_in_flight_are_counted_and_go_stale_after_60_s() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut a = EngineActivity::default();
        a.command_sent("screenshot", t0);
        a.command_sent("screenshot", t0 + s(1));
        a.on_event(&json!({"type":"screenshot","path":"a.png"}), 0.0);
        assert!(a.busy(t0 + s(1)), "the second PNG is still being written");
        assert!(a.retarget_busy(t0 + s(1)).saving, "and it holds a retarget");
        a.on_event(&json!({"type":"screenshot_failed","reason":"r"}), 0.0);
        assert!(!a.busy(t0 + s(1)), "both reported");
        a.on_event(&json!({"type":"screenshot","path":"x.png"}), 0.0);
        assert!(!a.busy(t0 + s(1)), "a stray report does not underflow");
        assert_eq!(SCREENSHOT_STALE, s(60), "a PNG can go to a sleeping hard drive");
        a.command_sent("screenshot", t0 + s(2));
        assert!(a.busy(t0 + s(2) + s(59)), "a slow disk is not a lost screenshot");
        assert!(!a.busy(t0 + s(2) + SCREENSHOT_STALE), "a lost one does not block forever");
    }

    /// Gate 20b P2-b: a start sent just before a session end counts as recording there.
    #[test]
    fn a_starting_long_recording_counts_for_a_session_end() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        assert!(!a.recording_or_starting());
        a.command_sent("record_start", t0);
        assert!(a.recording_or_starting() && !a.is_recording());
        a.on_event(&json!({"type": "recording_started", "path": "E:/G__rec.mp4"}), 1.0);
        assert!(a.recording_or_starting());
        a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4"}), 2.0);
        assert!(!a.recording_or_starting());
    }

    /// Gate 20b v3 P1: a stop from any sender is in flight until `recording_stopped` (a
    /// continued part's stop is not the end); a stop with nothing recording is not.
    #[test]
    fn a_stop_is_in_flight_until_the_recording_reports_stopping() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        a.stop_sent();
        assert!(!a.stop_in_flight(), "nothing to stop");
        a.command_sent("record_start", t0);
        a.on_event(&json!({"type": "recording_started", "path": "E:/G__rec.mp4"}), 1.0);
        a.command_sent("record_toggle", t0);
        assert!(a.stop_in_flight() && a.is_recording(), "the toggle stops it; still Active until the engine says so");
        a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4", "continued": true}), 2.0);
        assert!(a.stop_in_flight(), "only a part ended");
        a.on_event(&json!({"type": "recording_stopped", "path": "E:/G__rec.mp4"}), 3.0);
        assert!(!a.stop_in_flight());
        a.command_sent("record_start", t0);
        a.stop_sent(); // a pause / retarget / sleep stop while starting
        assert!(a.stop_in_flight() && a.is_starting());
        a.on_event(&json!({"type": "recording_failed", "reason": "x"}), 4.0);
        assert!(!a.stop_in_flight());
    }

    #[test]
    fn a_fresh_engine_clears_everything() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        a.save_sent(t0);
        a.on_event(&json!({"type":"recording_started","path":"p"}), 1.0);
        a.engine_restarted();
        assert_eq!(a, EngineActivity::default());
    }

    #[test]
    fn a_continued_recording_stays_active_between_its_parts() {
        let t0 = Instant::now();
        let mut a = EngineActivity::default();
        a.on_event(&json!({"type":"recording_started","path":"p1","continued":false}), 5.0);
        a.on_event(&json!({"type":"recording_stopped","path":"p1","seconds":9.0,"bytes":1,"continued":true}), 9.0);
        assert!(a.is_recording(), "no gap between the parts for a retarget to slip into");
        assert!(a.retarget_busy(t0).long_rec);
        a.on_event(&json!({"type":"recording_started","path":"p2","continued":true}), 9.5);
        assert_eq!(a.long, LongRec::Active { path: "p2".into(), started_unix: 5.0 }, "the timer counts from part 1");
        // A retarget's stop sent before the size change is answered by the last part's stop.
        assert!(a.auto_stop(t0, false));
        let ev = a.on_event(&json!({"type":"recording_stopped","path":"p2","seconds":1.0,"bytes":1,"continued":true}), 10.0);
        assert_ne!(ev, Some(Trigger::AutoStopped { game_closed: false }));
        assert!(a.is_recording());
        let ev = a.on_event(&json!({"type":"recording_stopped","path":"p3","seconds":1.0,"bytes":1,"continued":false}), 11.0);
        assert_eq!(ev, Some(Trigger::AutoStopped { game_closed: false }));
        assert_eq!(a.long, LongRec::Idle);
    }
}

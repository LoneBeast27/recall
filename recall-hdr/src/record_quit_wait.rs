//! A quit (tray Quit, the control pipe's `quit` from the installer or a script, WM_QUIT,
//! a session end) while the engine still writes a save: the engine's quit would abort it
//! and the clip would be gone, so the quit waits for the save first. Up to
//! [`QUIT_SAVE_WAIT`], and on while the save keeps making progress (bounded by the save's
//! own hold cap, `record_activity_writing::hold_cap`). The island says "Finishing the clip
//! before quitting" meanwhile. A save still in flight when the wait runs out is reported
//! once as "Save failed: Recall was closed during the save", then the quit goes on. So is a
//! save the timers already gave up on (past its cap, or no answer in time) but the engine has
//! not answered: the quit no longer goes on as if nothing were in flight (design P16; it used
//! to cut such a save 8 s later with no report). All on the awake-only clock.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::awake_clock::Awake;
use crate::log;
use crate::record_activity::EngineActivity;
use crate::record_quit::QuitReason;

/// The least a quit waits for a save still being written.
pub const QUIT_SAVE_WAIT: Duration = Duration::from_secs(60);
/// The island notice while a quit waits.
pub const QUIT_WAIT_TITLE: &str = "Finishing the clip before quitting";
pub const QUIT_WAIT_DETAIL: &str = "Recall quits once it is saved. Quit again to stop now";
/// Why a save the quit gave up on failed.
pub const CLOSED_DURING_SAVE: &str = "Recall was closed during the save";

/// What a quit does this pump tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitStep {
    /// A save is still being written: keep the engine (`first`: the wait starts now).
    Wait { first: bool },
    /// Nothing is being written: quit now.
    Go,
    /// The wait ran out with this many saves in flight: report them, then quit.
    Lost(u32),
}

/// The quit's wait, from the first tick a quit was asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuitWait {
    started: Option<Awake>,
    /// The user asked to quit again during the wait: quit now (the save is reported lost).
    forced: bool,
    /// When the first quit was asked through [`on_quit_request`] (else the wait's start).
    asked_at: Option<Awake>,
    /// A quit asked while a sign-out waits: it goes on if the sign-out is cancelled.
    also: Option<QuitReason>,
    /// A `save_first` quit already sent its saves (`record_quit_save`): a repeat sends none.
    saved_first: bool,
}

/// A 2nd tray Quit forces only this long after the first (a double click just joins).
pub const TRAY_FORCE_GAP: Duration = Duration::from_secs(1);

/// How long the pump sleeps per pass while a quit waits (no busy-spin on a passed deadline).
pub const QUIT_PUMP_WAIT_MS: u32 = 250;

/// The pump's sleep this pass: up to `cap` until the next deadline, but while a quit
/// waits for a save the deadlines are not served (`tick` does not run), so a passed
/// one must not turn the wait into a busy-spin: sleep [`QUIT_PUMP_WAIT_MS`] instead.
pub fn pump_wait_ms(quitting: bool, deadline: Option<Instant>, now: Instant, cap: u32) -> u32 {
    if quitting {
        return QUIT_PUMP_WAIT_MS;
    }
    crate::app::wait_ms_at(deadline, now, cap)
}

/// A quit asked while `current` may already wait: the reason the quit now has. It quits
/// at once (`forced`) only when it says so (`force`: the pipe's `"force":true`), or for a
/// tray Quit during the wait at least [`TRAY_FORCE_GAP`] after the first quit. A repeated
/// plain pipe quit (an installer re-run, a double click in the app) just joins the wait.
/// A quit asked while a sign-out waits is kept for [`resume_after_session_end`].
pub fn on_quit_request(current: Option<QuitReason>, asked: QuitReason, force: bool, now: Awake, wait: &mut QuitWait) -> Option<QuitReason> {
    let Some(current) = current else {
        wait.asked_at = Some(now);
        wait.forced |= force;
        return Some(asked);
    };
    let first = wait.asked_at.or(wait.started).unwrap_or(now);
    let tray_again = asked == QuitReason::TrayMenu && wait.waiting() && now.saturating_duration_since(first) >= TRAY_FORCE_GAP;
    if force || tray_again {
        wait.forced = true;
    }
    if current == QuitReason::SessionEnd && asked != QuitReason::SessionEnd {
        wait.also = Some(asked);
    }
    Some(if asked == QuitReason::TrayMenu { asked } else { current })
}

/// The pipe's `{"cmd":"quit","force":true}`: stop now, even during a save.
pub fn quit_force(cmd: &Value) -> bool {
    cmd.get("force").and_then(Value::as_bool).unwrap_or(false)
}

/// The sign-out was cancelled: the quit it started is called off, but a quit asked
/// meanwhile (the installer, the tray) goes on, waiting afresh.
pub fn resume_after_session_end(wait: &mut QuitWait) -> Option<QuitReason> {
    let kept = wait.also.take();
    wait.reset();
    kept
}

/// The quit under way is a held sign-out's alone (no other quit was asked): a cancelled
/// sign-out calls it off, so an installer must still send its own plain quit, which joins the
/// wait and is kept through a cancel ([`on_quit_request`]). Reported as `quit_for_sign_out`.
pub fn sign_out_only(quit: Option<QuitReason>, wait: &QuitWait) -> bool {
    quit == Some(QuitReason::SessionEnd) && wait.also.is_none()
}

/// The control pipe's view of a quit waiting for a save: `finishing_clip` (true while a
/// quit was asked and a save is still written) and `finishing_progress` (`{"saves": N,
/// "step": S, "bytes": B}`, `step` = the engine's progress mark, `bytes` = the bytes
/// written so far, each null without a report; null when not finishing).
pub fn finishing_fields(finishing: bool, saves: u32, mark: Option<u64>, bytes: Option<u64>) -> Value {
    let progress = finishing.then(|| json!({"saves": saves, "step": mark, "bytes": bytes}));
    json!({"finishing_clip": finishing, "finishing_progress": progress})
}

/// The saves a quit gives up on: every unanswered one, at least one while a save still writes
/// (a `save_first` quit held only by its `recording_stopped` loses no save of its own).
fn lost_at_give_up(activity: &EngineActivity, saving: bool) -> u32 {
    if saving { activity.unanswered().max(1) } else { activity.unanswered() }
}

impl QuitWait {
    /// This tick's step: go once nothing is in flight; wait [`QUIT_SAVE_WAIT`] from the
    /// first tick, and on while the save still makes progress; then give up on it. A save the
    /// engine never answered that no longer counts as writing (past its cap, or expired) is
    /// reported lost at once (design P16), never a silent go.
    pub fn step(&mut self, activity: &mut EngineActivity, now: Awake) -> QuitStep {
        let saving = activity.saves_writing(now);
        // A `save_first` quit's `record_stop` saves the long recording when
        // `recording_stopped` comes: that quit waits for it as for a save.
        if !saving && !(self.saved_first && activity.stop_in_flight()) {
            return match activity.unanswered() {
                0 => QuitStep::Go,
                lost => QuitStep::Lost(lost),
            };
        }
        if self.forced {
            return QuitStep::Lost(lost_at_give_up(activity, saving));
        }
        let first = self.started.is_none();
        let started = *self.started.get_or_insert(now);
        if now.saturating_duration_since(started) < QUIT_SAVE_WAIT || activity.save_growing(now) {
            QuitStep::Wait { first }
        } else {
            QuitStep::Lost(lost_at_give_up(activity, saving))
        }
    }

    /// The quit was called off (a cancelled session end): a later quit waits afresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The first `save_first` of this quit: true once (it sends the saves), false after.
    pub fn take_save_first(&mut self) -> bool {
        !std::mem::replace(&mut self.saved_first, true)
    }

    /// A `save_first` quit waits for its long recording's `recording_stopped`.
    pub fn holds_for_stop(&self, activity: &EngineActivity) -> bool {
        self.saved_first && activity.stop_in_flight()
    }

    /// A wait for a save is under way (its first tick passed).
    pub fn waiting(&self) -> bool {
        self.started.is_some()
    }
}

/// Copy `fields` (an object) into the status object `status`.
pub fn merge_fields(status: &mut Value, fields: Value) {
    if let (Some(status), Value::Object(fields)) = (status.as_object_mut(), fields) {
        status.extend(fields);
    }
}

/// The answer to the control pipe's `quit`: `{"type":"quitting","finishing_clip":..,"finishing_progress":..}`.
pub fn quitting_event(fields: Value) -> String {
    let mut event = json!({"type": "quitting"});
    merge_fields(&mut event, fields);
    event.to_string()
}

/// The `save_failed` each save the quit gave up on gets (mod `engine`, as the engine's own).
pub fn closed_during_save_event() -> String {
    json!({"type": "save_failed", "reason": CLOSED_DURING_SAVE}).to_string()
}

impl crate::record_app::Controller {
    /// Pump step once a quit was asked: true = shut down now. While a save is still
    /// written the engine keeps running; a save the wait gave up on is reported once
    /// (the engine's own abort failure is not broadcast after it).
    /// `_now` is kept for its callers (`pump`, and fix/deep-overlay-r9's `finish_saves`): the
    /// wait reads the awake-only clock itself.
    pub(crate) fn quit_ready(&mut self, _now: Instant) -> bool {
        let now = self.awake();
        match self.quit_wait.step(&mut self.activity, now) {
            QuitStep::Go => true,
            QuitStep::Wait { first } => {
                if first {
                    log!("record: quit -- waiting for the save being written (at least {} s, on while it grows)", QUIT_SAVE_WAIT.as_secs());
                    self.shortcut_notice(QUIT_WAIT_TITLE, QUIT_WAIT_DETAIL);
                }
                false
            }
            QuitStep::Lost(lost) => {
                log!("record: quit -- {lost} save(s) still being written after the wait; quitting anyway (reported failed)");
                self.supervisor.detach_hub();
                for _ in 0..lost {
                    self.hub.event("engine", &closed_during_save_event());
                }
                true
            }
        }
    }

    /// A quit asked by `asked` (tray, pipe; `force`: the pipe's `"force":true`).
    pub(crate) fn ask_quit(&mut self, asked: QuitReason, force: bool) {
        let (before, was_forced) = (self.quit, self.quit_wait.forced);
        let now = self.awake();
        self.quit = on_quit_request(before, asked, force, now, &mut self.quit_wait);
        if self.quit_wait.forced && !was_forced {
            log!("record: {} {}-- quitting now, a save in flight is lost", asked.label(), if force { "with force " } else { "again during the save wait " });
        } else if before.is_some() {
            log!("record: {} joins the quit already waiting", asked.label());
        }
    }

    /// `finishing_fields` now: a quit was asked and a save is still being written.
    pub(crate) fn finishing_now(&mut self) -> Value {
        let now = self.awake();
        let finishing = self.quit.is_some() && (self.activity.saves_writing(now) || self.quit_wait.holds_for_stop(&self.activity));
        let a = &self.activity;
        finishing_fields(finishing, a.saves_in_flight(), a.writing_mark(), a.writing_bytes())
    }

    /// While a quit waits: engine events and the status only (no retarget, no restart).
    pub(crate) fn quitting_tick(&mut self, now: Instant) {
        self.drain_engine_events(now);
        self.publish_status();
    }
}

#[cfg(test)]
#[path = "record_quit_wait_tests.rs"]
mod more_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn writing(step: u64) -> Value {
        json!({"type":"status","save_writing":{"bytes":0,"queued":0,"step":step}})
    }

    #[test]
    fn a_quit_waits_for_the_save_being_written_then_goes() {
        let t0 = Awake::ZERO;
        let s = Duration::from_secs;
        let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
        assert_eq!(q.step(&mut a, t0), QuitStep::Go, "nothing saving: quit at once");
        a.save_sent(t0);
        assert_eq!(q.step(&mut a, t0 + s(1)), QuitStep::Wait { first: true }, "the island says so once");
        assert_eq!(q.step(&mut a, t0 + s(2)), QuitStep::Wait { first: false });
        a.on_engine_event(&json!({"type":"saved","path":"G__x.mp4"}), 0.0, t0 + s(5));
        a.on_engine_event(&json!({"type":"status"}), 0.0, t0 + s(6));
        assert_eq!(q.step(&mut a, t0 + s(6)), QuitStep::Go, "saved: the quit goes on");
    }

    #[test]
    fn a_quit_waits_60_s_then_on_while_the_save_grows_and_reports_a_loss_after() {
        let t0 = Awake::ZERO;
        let s = Duration::from_secs;
        let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
        a.save_sent(t0);
        // The save grows every second until 100 s, then stops (a wedged writer).
        let mut out = Vec::new();
        for sec in 0..=200u64 {
            let now = t0 + s(sec);
            if sec <= 100 {
                a.on_engine_event(&writing(sec + 1), 0.0, now);
            }
            out.push(q.step(&mut a, now));
        }
        assert!(out[..110].iter().all(|o| matches!(o, QuitStep::Wait { .. })), "60 s, then on while growing (+10 s)");
        let lost = out.iter().position(|o| matches!(o, QuitStep::Lost(_))).expect("the wait ends");
        assert_eq!(lost, 110, "growth stopped at 100 s: 10 s later the quit gives up");
        assert_eq!(out[lost], QuitStep::Lost(1));
        let line: Value = serde_json::from_str(&closed_during_save_event()).unwrap();
        assert_eq!(line, json!({"type":"save_failed","reason":"Recall was closed during the save"}));
    }

    #[test]
    fn a_save_with_no_progress_report_gives_the_quit_60_s() {
        // An engine without `save_writing`: only the counted save, 60 s.
        let t0 = Awake::ZERO;
        let (mut a, mut q) = (EngineActivity::default(), QuitWait::default());
        a.save_sent(t0);
        let quit = t0 + Duration::from_secs(1); // the quit is asked 1 s after the save
        assert_eq!(q.step(&mut a, quit), QuitStep::Wait { first: true });
        assert!(matches!(q.step(&mut a, quit + QUIT_SAVE_WAIT - Duration::from_millis(1)), QuitStep::Wait { .. }));
        assert_eq!(q.step(&mut a, quit + QUIT_SAVE_WAIT), QuitStep::Lost(1), "60 s from the quit, no growth to wait on");
    }
}

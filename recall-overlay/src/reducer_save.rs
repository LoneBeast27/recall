//! Clip saves as the reducer follows them. The engine writes saves one at a
//! time and answers oldest first, so only the save in front waits: its wait
//! starts when it is sent or when the save ahead of it answers. Past
//! `SAVING_FOR` the ring says "Still saving…"; one that never hears back by
//! `CONFIRM_FOR` says "Save not confirmed" instead of vanishing, and so does
//! every save queued behind it (one window per engine answer, as the
//! controller's `SAVE_STALE`, never one per save). The engine's status reports the
//! bytes of the save it is writing (`save_writing`): while they grow the front
//! save is alive, so its ring stays ("Still saving…" past `SAVING_FOR`) and the
//! `CONFIRM_FOR` wait counts from the last growth. A refusal
//! (`refused`: never queued) takes no save's place, and an engine rejection
//! (`rejected`: failed before it was queued for writing) is the newest save's. While a save is still in flight after another's outcome (or
//! after any other notice), the island goes back to its spinning ring. A
//! second outcome while the first one's notice still shows counts up ("2
//! clips saved"), so it reads as a new result. "Clip saved" keeps the game
//! name "Saving" showed, so only the title word changes when the clip lands.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use super::{truncate, Reducer, CONFIRM_FOR, FAILURE_FOR, SAVED_FOR, SAVING_FOR};
use crate::notice::{NoticeView, SavePhase, Tone};

/// The detail line's length cap ("<game> · m:ss").
const DETAIL_MAX: usize = 40;
/// What an unanswered save says.
pub const UNCONFIRMED: &str = "The recorder did not answer";
/// The ring's title for a save past `SAVING_FOR` (still waiting, not failed).
pub const STILL_SAVING: &str = "Still saving…";

/// One save the controller sent that the engine has not answered.
#[derive(Clone, Debug)]
struct Pending {
    game: String,
    secs: u64,
    /// When its wait began: sent, or when the save ahead of it answered.
    since: Instant,
}

/// What a save's outcome notice says (outcomes of one kind count up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Saved,
    Failed,
    Unconfirmed,
}

impl Outcome {
    fn title(self, n: u32) -> String {
        match (self, n) {
            (Outcome::Saved, 1) => "Clip saved".into(),
            (Outcome::Saved, n) => format!("{n} clips saved"),
            (Outcome::Failed, 1) => "Save failed".into(),
            (Outcome::Failed, n) => format!("{n} saves failed"),
            (Outcome::Unconfirmed, 1) => "Save not confirmed".into(),
            (Outcome::Unconfirmed, n) => format!("{n} saves not confirmed"),
        }
    }
}

/// Saves in flight, oldest first, and the outcome notice showing now.
#[derive(Clone, Debug, Default)]
pub(super) struct Saves {
    pending: VecDeque<Pending>,
    shown: Option<(Outcome, u32)>,
    /// `status.save_writing.bytes` last seen, and when they last changed.
    writing: Option<u64>,
    grew: Option<Instant>,
}

impl Saves {
    /// The save in front has had no answer for `CONFIRM_FOR`: the engine is
    /// not answering, so the whole queue is unconfirmed. Returns how many.
    fn overdue(&mut self, now: Instant) -> u32 {
        if self.front_wait().is_none_or(|since| now < since + CONFIRM_FOR) {
            return 0;
        }
        let lost = self.pending.len() as u32;
        self.pending.clear();
        lost
    }

    /// When the front save last showed life: sent, the save ahead answered, or the
    /// bytes being written grew since.
    fn front_wait(&self) -> Option<Instant> {
        let since = self.pending.front()?.since;
        Some(self.grew.map_or(since, |grew| grew.max(since)))
    }

    /// One engine status: true when the save being written grew (a save appeared,
    /// or its bytes changed) while a save waits here.
    fn progress(&mut self, status: &Value, now: Instant) -> bool {
        let seen = status.get("save_writing").map(|w| w.get("bytes").and_then(Value::as_u64).unwrap_or(0));
        let grew = seen.is_some() && seen != self.writing && !self.pending.is_empty();
        self.writing = seen;
        if grew {
            self.grew = Some(now);
        }
        grew
    }

    /// The save now in front is the one being written: its wait starts now.
    fn restart_front(&mut self, now: Instant) {
        if let Some(p) = self.pending.front_mut() {
            p.since = now;
        }
    }

    /// The save an outcome answers: none for a refusal, the newest for an
    /// engine rejection, else the oldest (and the next one starts its wait).
    fn answer(&mut self, event: &Value, now: Instant) -> Option<Pending> {
        let flag = |key: &str| event.get(key).and_then(Value::as_bool) == Some(true);
        if flag("refused") {
            return None;
        }
        if flag("rejected") && self.pending.len() > 1 {
            return self.pending.pop_back();
        }
        let answered = self.pending.pop_front();
        self.restart_front(now);
        answered
    }

    /// The save in front has waited past `SAVING_FOR`.
    fn slow(&self, now: Instant) -> bool {
        self.pending.front().is_some_and(|p| now >= p.since + SAVING_FOR)
    }
}

/// "<game> · m:ss" in at most 40 characters: a long name gives way, never the length.
pub fn save_detail(game: &str, secs: u64) -> String {
    if secs == 0 {
        return truncate(game, DETAIL_MAX);
    }
    let length = format!(" · {}:{:02}", secs / 60, secs % 60);
    let room = DETAIL_MAX.saturating_sub(length.chars().count()).max(1);
    format!("{}{length}", truncate(game, room))
}

/// The game name the file name carries (the engine's slug), for a `saved`
/// that no `saving` announced.
fn slug_name(event: &Value) -> String {
    let filename = event
        .get("path")
        .and_then(Value::as_str)
        .and_then(|p| Path::new(p).file_name())
        .and_then(|p| p.to_str())
        .unwrap_or("clip");
    filename.split("__").next().unwrap_or(filename).replace(['-', '_'], " ")
}

impl Reducer {
    /// `{"type":"saving","seconds":N}`: the controller sent a save. "Saving"
    /// over "<game> · m:ss" with the spinning ring.
    pub(super) fn apply_saving(&mut self, event: &Value, now: Instant) -> bool {
        let secs = event.get("seconds").and_then(Value::as_u64).unwrap_or(0);
        let name = self.session.target_name.trim();
        let game = if !name.is_empty() { name } else if self.session.desktop { "Desktop" } else { "clip" };
        self.saves.pending.push_back(Pending { game: game.to_string(), secs, since: now });
        self.show_saving(now)
    }

    /// An engine status: a save whose bytes grow keeps its ring (and its wait) going.
    pub(super) fn apply_save_progress(&mut self, status: &Value, now: Instant) -> bool {
        let ring = self.transient.as_ref().and_then(|t| t.notice.as_ref()).is_some_and(|n| n.save == Some(SavePhase::Saving));
        self.saves.progress(status, now) && ring && self.show_saving(now)
    }

    /// The newest save in flight: its ring ("Still saving…" once the save in
    /// front is slow), held until the front save's wait ends.
    fn show_saving(&mut self, now: Instant) -> bool {
        let Some(p) = self.saves.pending.back() else { return false };
        let detail = save_detail(&p.game, p.secs);
        let until = self.saves.front_wait().map_or(now, |since| since + CONFIRM_FOR);
        let (tone, title) = if self.saves.slow(now) { (Tone::Neutral, STILL_SAVING) } else { (Tone::Recording, "Saving") };
        let notice = NoticeView::new(tone, title, detail.clone(), true).emphasized().saving(SavePhase::Saving);
        self.set_transient(title, detail, until, notice)
    }

    /// Back to the ring when nothing else shows and a save is still in flight.
    pub(super) fn resume_saving(&mut self, now: Instant) -> bool {
        self.transient.is_none() && self.show_saving(now)
    }

    /// The count for this outcome: one more when the same kind of outcome
    /// still shows, else 1.
    fn outcome_count(&mut self, kind: Outcome, add: u32, now: Instant) -> u32 {
        let showing = self.transient.as_ref().is_some_and(|t| {
            now < t.until && t.notice.as_ref().is_some_and(|n| n.save.is_some_and(|s| s != SavePhase::Saving))
        });
        let n = match self.saves.shown {
            Some((shown, n)) if showing && shown == kind => n + add,
            _ => add,
        };
        self.saves.shown = Some((kind, n));
        n
    }

    /// The engine's `saved`: "Clip saved" over the name the ring showed.
    pub(super) fn apply_saved(&mut self, event: &Value, now: Instant) -> bool {
        let pending = self.saves.answer(event, now);
        let secs = event.get("seconds").and_then(Value::as_f64).unwrap_or(0.0).max(0.0).round() as u64;
        let game = pending.map_or_else(|| slug_name(event), |p| p.game);
        let detail = save_detail(&game, secs);
        let title = Outcome::Saved.title(self.outcome_count(Outcome::Saved, 1, now));
        let notice = NoticeView::new(Tone::Success, &title, detail.clone(), true).emphasized().saving(SavePhase::Saved);
        self.set_transient(&title, detail, now + SAVED_FOR, notice)
    }

    /// `save_failed` (the engine's, or the controller's: a refusal, or the
    /// engine stopped with the save in flight).
    pub(super) fn apply_save_failed(&mut self, event: &Value, now: Instant) -> bool {
        self.saves.answer(event, now);
        let reason = truncate(event.get("reason").and_then(Value::as_str).unwrap_or("unknown error"), 44);
        self.show_failure(Outcome::Failed, 1, reason, now)
    }

    fn show_failure(&mut self, kind: Outcome, add: u32, reason: String, now: Instant) -> bool {
        let title = kind.title(self.outcome_count(kind, add, now));
        let notice = NoticeView::new(Tone::Failure, &title, reason.clone(), false).emphasized().saving(SavePhase::Failed);
        self.set_transient(&title, format!("{title}: {reason}"), now + FAILURE_FOR, notice)
    }

    /// Each tick: saves whose wait ran out say so, and the ring turns to
    /// "Still saving…" (or back, when the next save starts its wait).
    pub(super) fn expire_saves(&mut self, now: Instant) -> bool {
        let lost = self.saves.overdue(now);
        if lost > 0 {
            return self.show_failure(Outcome::Unconfirmed, lost, UNCONFIRMED.into(), now);
        }
        let ring = self.transient.as_ref().and_then(|t| t.notice.as_ref()).filter(|n| n.save == Some(SavePhase::Saving));
        match ring.map(|n| n.title == STILL_SAVING) {
            Some(shown) if shown != self.saves.slow(now) => self.show_saving(now),
            _ => false,
        }
    }

    pub(super) fn clear_saves(&mut self) {
        self.saves = Saves::default();
    }
}

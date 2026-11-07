//! Clip saves as the reducer follows them. Each `saving` the controller sends
//! waits for its own outcome (the engine answers oldest first) for
//! `SAVING_FOR`; one that never hears back says "Save not confirmed" instead
//! of vanishing. While a save is still in flight after another's outcome (or
//! after any other notice), the island goes back to its spinning ring. A
//! second outcome while the first one's notice still shows counts up ("2
//! clips saved"), so it reads as a new result. "Clip saved" keeps the game
//! name "Saving" showed, so only the title word changes when the clip lands.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use super::{truncate, Reducer, FAILURE_FOR, SAVED_FOR, SAVING_FOR};
use crate::notice::{NoticeView, SavePhase, Tone};

/// The detail line's length cap ("<game> · m:ss").
const DETAIL_MAX: usize = 40;
/// What an unanswered save says.
pub const UNCONFIRMED: &str = "The recorder did not answer";

/// One save the controller sent that the engine has not answered.
#[derive(Clone, Debug)]
struct Pending {
    game: String,
    secs: u64,
    until: Instant,
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
}

impl Saves {
    /// Drops the saves whose wait ran out; returns how many.
    fn overdue(&mut self, now: Instant) -> u32 {
        let mut lost = 0;
        while self.pending.front().is_some_and(|p| now >= p.until) {
            self.pending.pop_front();
            lost += 1;
        }
        lost
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
        self.saves.pending.push_back(Pending { game: game.to_string(), secs, until: now + SAVING_FOR });
        self.show_saving()
    }

    /// The newest save in flight: its ring, held until the last wait ends.
    fn show_saving(&mut self) -> bool {
        let Some(p) = self.saves.pending.back() else { return false };
        let (detail, until) = (save_detail(&p.game, p.secs), p.until);
        let notice = NoticeView::new(Tone::Recording, "Saving", detail.clone(), true).emphasized().saving(SavePhase::Saving);
        self.set_transient("Saving", detail, until, notice)
    }

    /// Back to the ring when nothing else shows and a save is still in flight.
    pub(super) fn resume_saving(&mut self) -> bool {
        self.transient.is_none() && self.show_saving()
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
        let pending = self.saves.pending.pop_front();
        let secs = event.get("seconds").and_then(Value::as_f64).unwrap_or(0.0).max(0.0).round() as u64;
        let game = pending.map_or_else(|| slug_name(event), |p| p.game);
        let detail = save_detail(&game, secs);
        let title = Outcome::Saved.title(self.outcome_count(Outcome::Saved, 1, now));
        let notice = NoticeView::new(Tone::Success, &title, detail.clone(), true).emphasized().saving(SavePhase::Saved);
        self.set_transient(&title, detail, now + SAVED_FOR, notice)
    }

    /// `save_failed` (the engine's, or the controller's when the engine
    /// stopped with the save in flight).
    pub(super) fn apply_save_failed(&mut self, event: &Value, now: Instant) -> bool {
        self.saves.pending.pop_front();
        let reason = truncate(event.get("reason").and_then(Value::as_str).unwrap_or("unknown error"), 44);
        self.show_failure(Outcome::Failed, 1, reason, now)
    }

    fn show_failure(&mut self, kind: Outcome, add: u32, reason: String, now: Instant) -> bool {
        let title = kind.title(self.outcome_count(kind, add, now));
        let notice = NoticeView::new(Tone::Failure, &title, reason.clone(), false).emphasized().saving(SavePhase::Failed);
        self.set_transient(&title, format!("{title}: {reason}"), now + FAILURE_FOR, notice)
    }

    /// Each tick: saves whose wait ran out say so.
    pub(super) fn expire_saves(&mut self, now: Instant) -> bool {
        match self.saves.overdue(now) {
            0 => false,
            lost => self.show_failure(Outcome::Unconfirmed, lost, UNCONFIRMED.into(), now),
        }
    }

    pub(super) fn clear_saves(&mut self) {
        self.saves = Saves::default();
    }
}

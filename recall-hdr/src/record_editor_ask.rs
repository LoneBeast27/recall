//! The island editor shortcut's answer. The controller sends the overlay
//! `{"type":"island_editor"}`; a current overlay answers on the control pipe
//! with `{"cmd":"island_editor_ack","started":B}`. An overlay too old to know
//! the editor answers nothing: after [`ACK_WITHIN`] the controller beeps, so
//! the shortcut never fails in silence. A refusal (`started` false) beeps too.

use std::time::{Duration, Instant};

/// How long a current overlay takes to answer, with room to spare.
pub const ACK_WITHIN: Duration = Duration::from_millis(1500);

#[derive(Debug, Default)]
pub struct EditorAsk {
    asked: Option<Instant>,
}

impl EditorAsk {
    /// The shortcut was sent to the overlay.
    pub fn ask(&mut self, now: Instant) {
        self.asked = Some(now);
    }

    /// The overlay answered. True = beep (it refused to start the editor).
    pub fn ack(&mut self, started: bool) -> bool {
        self.asked = None;
        !started
    }

    /// Each tick: true once when the overlay never answered (beep).
    pub fn unanswered(&mut self, now: Instant) -> bool {
        let late = self.asked.is_some_and(|at| now.saturating_duration_since(at) >= ACK_WITHIN);
        if late {
            self.asked = None;
        }
        late
    }
}

/// The notice when the game runs in exclusive fullscreen (nothing can be dragged there).
pub fn borderless_notice_json() -> String {
    crate::record_notice::shortcut_notice_json(
        "Island editor needs Borderless",
        "The game runs in exclusive fullscreen. Set it to Borderless to place the island.",
        true,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_overlay_that_never_answers_gets_one_beep() {
        let t0 = Instant::now();
        let mut ask = EditorAsk::default();
        assert!(!ask.unanswered(t0 + ACK_WITHIN), "nothing asked");
        ask.ask(t0);
        assert!(!ask.unanswered(t0 + ACK_WITHIN - Duration::from_millis(1)));
        assert!(ask.unanswered(t0 + ACK_WITHIN), "no answer: beep");
        assert!(!ask.unanswered(t0 + ACK_WITHIN * 3), "once");
    }

    #[test]
    fn an_answer_stops_the_beep_and_a_refusal_beeps_at_once() {
        let t0 = Instant::now();
        let mut ask = EditorAsk::default();
        ask.ask(t0);
        assert!(!ask.ack(true), "started: quiet");
        assert!(!ask.unanswered(t0 + ACK_WITHIN * 2));
        ask.ask(t0);
        assert!(ask.ack(false), "refused: beep");
        assert!(!ask.unanswered(t0 + ACK_WITHIN * 2), "and no second beep");
    }

    #[test]
    fn the_borderless_notice_says_what_to_change() {
        let notice = borderless_notice_json();
        assert!(notice.contains("Borderless") && notice.contains("exclusive fullscreen"), "{notice}");
    }
}

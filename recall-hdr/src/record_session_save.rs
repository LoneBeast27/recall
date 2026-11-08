//! Sign-out / shutdown while a save writes (R5-overlay-5). Windows ends every process
//! at a session end, so the save in flight would be lost without a word. While the
//! engine writes a save, the controller holds a shutdown block reason ("Recall is
//! finishing a clip", `ShutdownBlockReasonCreate`) and answers `WM_QUERYENDSESSION` with
//! "wait": Windows shows the reason and lets the user wait or end anyway. The controller
//! then quits the way a tray Quit does (`record_quit_wait`: up to 60 s, on while the save
//! grows, bounded), and its exit lets the session end go on. A session end the user
//! cancels (`WM_ENDSESSION` with `FALSE`) calls the quit off. Never an endless block:
//! the quit's wait is bounded, and Windows always offers "end anyway".
//!
//! The shared `record_session_win` window applies these decisions.

/// What Windows shows while Recall holds a session end.
pub const BLOCK_REASON: &str = "Recall is finishing a clip";

/// What the session-end window saw, queued for the pump by its window procedure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionNotice {
    /// `WM_QUERYENDSESSION`; `held`: it was answered "wait" (a save was being written).
    Query { held: bool },
    /// `WM_ENDSESSION`; `ending`: false when the session end was cancelled.
    End { ending: bool },
}

/// What the pump does about a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Nothing,
    /// Quit like a tray Quit (it waits for the save first).
    Quit,
    /// The session end was cancelled: call the quit it asked for off.
    Resume,
}

/// The block reason's life: on while a save is written, off once none is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockReason {
    on: bool,
}

impl BlockReason {
    /// `saving` now: `Some(on)` when the reason must be created (`true`) or destroyed.
    pub fn update(&mut self, saving: bool) -> Option<bool> {
        (saving != self.on).then(|| {
            self.on = saving;
            saving
        })
    }

    pub fn on(&self) -> bool {
        self.on
    }
}

/// `WM_QUERYENDSESSION`'s answer: "wait" (`false`) only while the reason is held.
pub fn query_answer(held: bool) -> bool {
    !held
}

/// A session end Recall held, until it quits or the user cancels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionEnd {
    held: bool,
}

impl SessionEnd {
    /// A held query quits once the save is done; a cancel after it calls that off.
    pub fn on_notice(&mut self, notice: SessionNotice) -> SessionAction {
        match notice {
            SessionNotice::Query { held: true } => {
                self.held = true;
                SessionAction::Quit
            }
            SessionNotice::End { ending: false } if self.held => {
                self.held = false;
                SessionAction::Resume
            }
            SessionNotice::Query { held: false } | SessionNotice::End { .. } => SessionAction::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_reason_is_held_exactly_while_a_save_is_written() {
        let mut r = BlockReason::default();
        assert_eq!(r.update(false), None, "nothing saving: no reason");
        assert_eq!(r.update(true), Some(true), "a save starts: create the reason");
        assert_eq!(r.update(true), None, "once");
        assert!(r.on());
        assert_eq!(r.update(false), Some(false), "saved: destroy it");
        assert_eq!(r.update(false), None);
    }

    #[test]
    fn a_session_end_during_a_save_waits_and_quits_after_it() {
        assert!(!query_answer(true), "a save in flight: Windows waits and shows the reason");
        assert!(query_answer(false), "nothing in flight: let the session end");
        let mut s = SessionEnd::default();
        assert_eq!(s.on_notice(SessionNotice::Query { held: false }), SessionAction::Nothing);
        assert_eq!(s.on_notice(SessionNotice::End { ending: true }), SessionAction::Nothing, "ends anyway: nothing to wait for");
        assert_eq!(s.on_notice(SessionNotice::Query { held: true }), SessionAction::Quit, "finish the save, then quit");
        assert_eq!(s.on_notice(SessionNotice::End { ending: false }), SessionAction::Resume, "cancelled: Recall keeps running");
        assert_eq!(s.on_notice(SessionNotice::End { ending: false }), SessionAction::Nothing, "once");
    }
}

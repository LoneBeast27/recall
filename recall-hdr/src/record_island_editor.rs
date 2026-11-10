//! The island editor shortcut and the overlay's answers to it (split out of
//! `record_actions.rs`, which stays under 500 lines; no behaviour change).

use std::time::Instant;

use serde_json::json;

use crate::log;
use crate::record_app::Controller;
use crate::record_notice::shortcut_notice_json;

impl Controller {
    /// The island editor shortcut: the overlay starts (or saves) it for the
    /// game in front. It only places the island in Dynamic mode, and only a
    /// running overlay can show it.
    pub(crate) fn island_editor(&mut self) {
        if self.cfg.overlay.corner != "dynamic" {
            let notice = shortcut_notice_json("Island editor needs Dynamic", "Set the island position to Dynamic", true, true);
            self.hub.event("session", &notice);
            return crate::supervisor::beep_failure();
        }
        if !self.overlay.running() {
            log!("record: island editor refused -- the overlay is not running (notices and stats off)");
            return crate::supervisor::beep_failure();
        }
        if crate::record_fullscreen::exclusive_now() {
            log!("record: island editor refused -- exclusive fullscreen (nothing can be dragged)");
            self.hub.event("session", &crate::record_editor_ask::borderless_notice_json());
            return crate::supervisor::beep_failure();
        }
        log!("record: island editor");
        self.editor_ask.ask(Instant::now());
        self.hub.event("session", &json!({"type": "island_editor"}).to_string());
    }

    /// The overlay's answer to the editor shortcut (`started` false: refused).
    pub(crate) fn island_editor_ack(&mut self, started: bool) {
        if self.editor_ask.ack(started) {
            log!("record: island editor refused by the overlay (see recall-overlay.log)");
            crate::supervisor::beep_failure();
        }
    }

    /// An overlay too old to know the editor never answers: beep once.
    pub(crate) fn island_editor_tick(&mut self, now: Instant) {
        if self.editor_ask.unanswered(now) {
            log!("record: island editor -- the overlay did not answer (an older overlay?)");
            crate::supervisor::beep_failure();
        }
    }
}

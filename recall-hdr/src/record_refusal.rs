//! The failure event a refused capture action gets (as if the engine had
//! failed it). A refused save was never queued and never announced, so it is
//! marked `refused`: the island shows the reason without ending a real
//! save's ring.

use serde_json::{json, Value};

pub fn refusal_event(cmd: &str, reason: &str) -> Value {
    match cmd {
        "screenshot" => json!({"type": "screenshot_failed", "reason": reason}),
        "save_clip" => json!({"type": "save_failed", "reason": reason, "refused": true}),
        _ => json!({"type": "recording_failed", "reason": reason, "path": null}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_save_is_marked_refused() {
        let save = refusal_event("save_clip", "Recording paused");
        assert_eq!(save, json!({"type": "save_failed", "reason": "Recording paused", "refused": true}));
        let shot = refusal_event("screenshot", "Recording paused");
        assert_eq!(shot, json!({"type": "screenshot_failed", "reason": "Recording paused"}));
    }

    /// `record_app::tick`: a new engine's hello wipes what was in flight, so
    /// the old engine's events still queued (its exit, which reports the
    /// saves it lost) are read first. The controller has no test harness;
    /// this pins the order in the live tick.
    #[test]
    fn the_hello_reset_reads_the_old_engines_exit_first() {
        let src = include_str!("record_app.rs");
        let branch = src.find("if hellos != self.seen_hello").expect("the hello branch");
        let reset = branch + src[branch..].find("self.activity.engine_restarted()").expect("the reset");
        assert!(src[branch..reset].contains("self.drain_engine_events(now)"), "drain before the reset");
    }
}

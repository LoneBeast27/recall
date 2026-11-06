//! Island notices for engine events that the session state does not cover: the replay buffer
//! restarting under a running game because Windows HDR turned on (the engine had to restart
//! capture in Rgba16F, `buffer_restarted`). Silent before, so a "save that!" found a buffer a
//! few seconds long with no hint why.

use serde_json::Value;

/// The engine's `buffer_restarted` reason when HDR turned on under a Bgra8 capture.
pub const HDR_ON: &str = "hdr_on";

/// The notice (title, detail) an engine event asks for, if any.
pub fn engine_notice(ev: &Value) -> Option<(&'static str, &'static str)> {
    if ev.get("type").and_then(Value::as_str)? != "buffer_restarted" {
        return None;
    }
    (ev.get("reason").and_then(Value::as_str)? == HDR_ON).then_some(("HDR turned on", "The replay buffer restarted"))
}

#[cfg(test)]
mod tests {
    use super::engine_notice;
    use serde_json::json;

    #[test]
    fn hdr_turning_on_under_a_bgra8_capture_shows_one_notice() {
        let ev = json!({"type": "buffer_restarted", "reason": "hdr_on"});
        assert_eq!(engine_notice(&ev), Some(("HDR turned on", "The replay buffer restarted")));
    }

    #[test]
    fn other_engine_events_show_nothing() {
        assert_eq!(engine_notice(&json!({"type": "saved", "path": "c.mp4"})), None);
        assert_eq!(engine_notice(&json!({"type": "buffer_restarted"})), None, "no reason, no notice");
        assert_eq!(engine_notice(&json!({"type": "buffer_restarted", "reason": "other"})), None);
    }
}

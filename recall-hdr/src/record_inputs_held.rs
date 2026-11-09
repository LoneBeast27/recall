//! The engine holds every audio track silent and opens no stream (`inputs_held`: its stdin was
//! not drained in time, or a read failed; merge-gate F7). The user is told by a notice, not only
//! by the Audio page meter: the microphones record nothing until the engine has its settings.

use serde_json::Value;

pub(crate) const TITLE: &str = "Microphones held";
pub(crate) const DETAIL: &str = "The recorder is waiting for its settings";

/// The session notice for an `inputs_held` engine event (shown even with notices off).
pub(crate) fn notice_for(ev: &Value) -> Option<String> {
    (ev.get("type").and_then(Value::as_str) == Some("inputs_held")).then(|| crate::record_notice::shortcut_notice_json(TITLE, DETAIL, true, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inputs_held_event_shows_a_notice() {
        let notice: Value = serde_json::from_str(&notice_for(&serde_json::json!({"type": "inputs_held", "reason": "the recorder is waiting for its settings"})).expect("a notice")).unwrap();
        assert_eq!((notice["title"].as_str(), notice["detail"].as_str(), notice["always"].as_bool()), (Some(TITLE), Some(DETAIL), Some(true)));
        assert_eq!(notice_for(&serde_json::json!({"type": "saved"})), None);
        assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"inputs_held"), "the supervisor hands it to the controller");
        let actions = include_str!("record_actions.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(actions.contains("crate::record_inputs_held::notice_for(&ev)"), "the engine-event loop shows it");
    }
}

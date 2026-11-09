//! The engine holds every audio track silent and opens no stream (`inputs_held`: its stdin was
//! not drained in time, or a read failed; merge-gate F7). The user is told by a notice, not only
//! by the Audio page meter: the microphones record nothing until the engine has its settings.
//!
//! Also the protocol floor's notice (`engine_outdated`, R1): the engine file is older than the
//! app, so it was given no microphone and no picked device. Shown even with notices off, and it
//! says why the mic is off.

use serde_json::Value;

pub(crate) const TITLE: &str = "Microphones held";
pub(crate) const DETAIL: &str = "The recorder is waiting for its settings";

/// The R1 notice, as decided (merge gate).
pub(crate) const OUTDATED_TITLE: &str = "Reinstall Recall: this recorder is older than the app; microphones are off";
pub(crate) const OUTDATED_DETAIL: &str = "Picked devices are off too. Game audio and the default output still record.";

/// The session notice for an engine event (shown even with notices off), or None.
pub(crate) fn notice_for(ev: &Value) -> Option<String> {
    match ev.get("type").and_then(Value::as_str)? {
        "inputs_held" => Some(crate::record_notice::shortcut_notice_json(TITLE, DETAIL, true, true)),
        crate::supervisor::floor::ENGINE_OUTDATED => Some(crate::record_notice::shortcut_notice_json(OUTDATED_TITLE, OUTDATED_DETAIL, true, true)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(ev: Value) -> Value {
        serde_json::from_str(&notice_for(&ev).expect("a notice")).unwrap()
    }

    #[test]
    fn an_inputs_held_event_shows_a_notice() {
        let notice = shown(serde_json::json!({"type": "inputs_held", "reason": "the recorder is waiting for its settings"}));
        assert_eq!((notice["title"].as_str(), notice["detail"].as_str(), notice["always"].as_bool()), (Some(TITLE), Some(DETAIL), Some(true)));
        assert_eq!(notice_for(&serde_json::json!({"type": "saved"})), None);
        assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"inputs_held"), "the supervisor hands it to the controller");
        let actions = include_str!("record_actions.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(actions.contains("crate::record_inputs_held::notice_for(&ev)"), "the engine-event loop shows it");
    }

    /// R1 condition 1: the floor's notice is a visible notice (even with notices off) that says
    /// why the mic is off, in the decided words.
    #[test]
    fn an_outdated_engine_shows_the_reinstall_notice() {
        let notice = shown(serde_json::json!({"type": crate::supervisor::floor::ENGINE_OUTDATED}));
        assert_eq!(notice["title"].as_str(), Some("Reinstall Recall: this recorder is older than the app; microphones are off"));
        assert_eq!(notice["always"].as_bool(), Some(true), "shown even with notices off");
    }
}

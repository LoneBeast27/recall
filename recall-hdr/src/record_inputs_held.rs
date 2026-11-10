//! The engine still holds its device tracks silent 2 s after its start (`inputs_held`: no
//! `sync` from the controller yet; merge-gate F7, design-audio-mute R-fresh-4). The user is told
//! by a notice, not only by the Audio page meter, and told again when they record
//! (`inputs_released`), so the held notice does not stand after the release.
//!
//! Also the protocol floor's notice (`engine_outdated`, R1): the engine file is older than the
//! app, so it was given no microphone and no picked device. Shown even with notices off, and it
//! says why the mic is off.

use serde_json::Value;

pub(crate) const TITLE: &str = "Audio held";
pub(crate) const DETAIL: &str = "The recorder is waiting for its settings: microphones and devices record nothing until then";
pub(crate) const RELEASED_TITLE: &str = "Audio recording";
pub(crate) const RELEASED_DETAIL: &str = "The recorder has its settings";

/// A held notice was shown and no release notice since.
static HELD_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The R1 notice, as decided (merge gate).
pub(crate) const OUTDATED_TITLE: &str = "Reinstall Recall: this recorder is older than the app; microphone and picked audio devices are off";
pub(crate) const OUTDATED_DETAIL: &str = "Game audio and the default output still record.";

/// The session notice for an engine event (shown even with notices off), or None.
pub(crate) fn notice_for(ev: &Value) -> Option<String> {
    match ev.get("type").and_then(Value::as_str)? {
        "inputs_held" => {
            HELD_SHOWN.store(true, std::sync::atomic::Ordering::SeqCst);
            Some(crate::record_notice::shortcut_notice_json(TITLE, DETAIL, true, true))
        }
        // Only after a held notice: a normal start says nothing.
        "inputs_released" => HELD_SHOWN.swap(false, std::sync::atomic::Ordering::SeqCst).then(|| crate::record_notice::shortcut_notice_json(RELEASED_TITLE, RELEASED_DETAIL, false, true)),
        crate::supervisor::floor::ENGINE_OUTDATED => Some(outdated_notice()),
        _ => None,
    }
}

/// The machine-readable action on the R1 notice: the app shows a Reinstall button for it
/// (Rohan 2026-11-10 22:00 UTC: "for half finished installs, ideal if we were to make user patch
/// it with clicking a reinstall button,").
pub(crate) const REINSTALL: &str = "reinstall";

fn outdated_notice() -> String {
    let mut v: Value = serde_json::from_str(&crate::record_notice::shortcut_notice_json(OUTDATED_TITLE, OUTDATED_DETAIL, true, true)).unwrap_or_default();
    v["action"] = REINSTALL.into();
    v.to_string()
}

/// `session.repair` (the app's Reinstall button): set while the engine is below the floor.
pub(crate) fn repair_json(below_floor: bool) -> Value {
    if below_floor { serde_json::json!({"title": OUTDATED_TITLE, "action": REINSTALL}) } else { Value::Null }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The held notice state is process-wide: these tests run one at a time.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn shown(ev: Value) -> Value {
        serde_json::from_str(&notice_for(&ev).expect("a notice")).unwrap()
    }

    #[test]
    fn an_inputs_held_event_shows_a_notice() {
        let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let notice = shown(serde_json::json!({"type": "inputs_held", "reason": "the recorder is waiting for its settings"}));
        assert_eq!((notice["title"].as_str(), notice["detail"].as_str(), notice["always"].as_bool()), (Some(TITLE), Some(DETAIL), Some(true)));
        assert_eq!(notice_for(&serde_json::json!({"type": "saved"})), None);
        assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"inputs_held"), "the supervisor hands it to the controller");
        let actions = include_str!("record_actions.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(actions.contains("crate::record_inputs_held::notice_for(&ev)"), "the engine-event loop shows it");
    }

    /// R-fresh-4: the release is told only after a held notice.
    #[test]
    fn a_release_after_a_held_notice_says_so_once() {
        let _one = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"inputs_released"));
        shown(serde_json::json!({"type": "inputs_held"}));
        assert_eq!(shown(serde_json::json!({"type": "inputs_released"}))["title"].as_str(), Some(RELEASED_TITLE));
        assert_eq!(notice_for(&serde_json::json!({"type": "inputs_released"})), None, "once");
    }

    /// R1 condition 1: the floor's notice is a visible notice (even with notices off) that says
    /// why the mic is off, in the decided words.
    #[test]
    fn an_outdated_engine_shows_the_reinstall_notice() {
        let notice = shown(serde_json::json!({"type": crate::supervisor::floor::ENGINE_OUTDATED}));
        assert_eq!(notice["title"].as_str(), Some("Reinstall Recall: this recorder is older than the app; microphone and picked audio devices are off"));
        assert_eq!(notice["always"].as_bool(), Some(true), "shown even with notices off");
        assert_eq!(notice["action"].as_str(), Some("reinstall"), "the app's Reinstall button");
        assert_eq!(repair_json(true)["action"].as_str(), Some("reinstall"));
        assert!(repair_json(false).is_null());
        let status = include_str!("record_cmds_status.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(status.contains("status[\"repair\"] = crate::record_inputs_held::repair_json(self.supervisor.engine_below_floor());"), "the session status carries it");
    }
}

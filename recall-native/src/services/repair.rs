//! Repairs the app offers the user. Rohan 2026-11-10 22:00 UTC, verbatim: "for half finished
//! installs, ideal if we were to make user patch it with clicking a reinstall button,". The
//! recorder says when its engine file is older than the app (`session.repair`, action
//! `reinstall`: it records no microphone or picked device), and the app shows a Reinstall button
//! on that notice. Every click goes through [`request_reinstall`].
//!
//! The merge gate owns Setup and the updater and wires [`request_reinstall`] to Setup's
//! `--update <install path>` mode. Until then it is a stub: it logs and runs nothing.

/// What the recorder's `session.repair` asks for: the notice and its action.
#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct RepairNotice {
    pub title: String,
    /// "reinstall": the engine file is older than the app.
    pub action: String,
}

/// The action that shows the Reinstall button.
pub const REINSTALL: &str = "reinstall";
/// The button's label.
pub const BUTTON: &str = "Reinstall";
/// What the stub answers.
pub const NOT_WIRED: &str = "not wired yet";

/// The notice to show with a Reinstall button, if the recorder asks for one.
pub fn reinstall_notice(repair: Option<&RepairNotice>) -> Option<&RepairNotice> {
    repair.filter(|r| r.action == REINSTALL)
}

/// Reinstall Recall (Setup's `--update <install path>`). NOT WIRED YET: the merge gate owns
/// Setup and wires this; it logs and returns [`NOT_WIRED`], and runs nothing.
pub fn request_reinstall() -> Result<(), &'static str> {
    eprintln!("[recall-native] repair: Reinstall clicked; request_reinstall is {NOT_WIRED} (the merge gate wires it to Setup --update)");
    Err(NOT_WIRED)
}

/// The Reinstall button's click: `reinstall` is [`request_reinstall`] in the app (a fake in
/// tests). The text to show when it did not start.
pub fn on_reinstall_click(reinstall: impl FnOnce() -> Result<(), &'static str>) -> Option<String> {
    reinstall().err().map(|why| format!("Reinstall: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The button's click calls the one reinstall function (a fake here: no Setup runs in a test).
    #[test]
    fn the_click_goes_through_the_one_reinstall_function() {
        let mut calls = 0;
        assert_eq!(on_reinstall_click(|| { calls += 1; Ok(()) }), None);
        assert_eq!(calls, 1);
        assert_eq!(on_reinstall_click(|| Err(NOT_WIRED)), Some("Reinstall: not wired yet".to_owned()));
        let banner = include_str!("../views/repair_banner.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(banner.contains("ui::button(\"reinstall-recall\", Variant::Primary).child(repair::BUTTON)"), "the button is on the notice");
        assert!(banner.contains("repair::on_reinstall_click(repair::request_reinstall)"), "its click calls the one function");
        let page = include_str!("../views/audio_page.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(page.contains("body.extend(self.repair_banner(cx));"), "the Audio page shows it");
    }

    /// The recorder's notice carries the action; only "reinstall" shows the button.
    #[test]
    fn the_session_repair_notice_shows_the_button() {
        let session: crate::services::daemon::SessionStatus = serde_json::from_value(serde_json::json!({
            "repair": {"title": "Reinstall Recall: this recorder is older than the app; microphone and picked audio devices are off", "action": "reinstall"}
        }))
        .unwrap();
        let notice = reinstall_notice(session.repair.as_ref()).expect("a button");
        assert!(notice.title.starts_with("Reinstall Recall"));
        assert_eq!(reinstall_notice(Some(&RepairNotice { title: "x".into(), action: "other".into() })), None);
        assert_eq!(reinstall_notice(None), None);
    }
}

//! The one-time notice before Recall first changes a Windows display setting on this machine.
//!
//! What can change (all behind the shelved HDR manager, [`crate::hdr_gate`], so a shipped
//! build changes none of them): Windows HDR on/off and the SDR content brightness of the
//! managed display, plus the NVIDIA colour clamp (NvAPI CSC). Each is journalled and put
//! back on exit, by the console handler, or by the next start after a hard kill (`journal`).
//!
//! Covered paths:
//! - HDR management started by the user's own switch in the app (a `set_config` from a
//!   client): the switch's label says what changes and that it is put back; flipping it is
//!   the acknowledgement.
//! - HDR management started without a user action (`behavior.hdr_auto` already on at start,
//!   or a hand edit of config.toml): nothing changes until this notice was shown on the
//!   island and in the app (a client must be connected to see it); then it is remembered.
//! - The Advanced Color default profile (`set_ac_profile`) is only ever the user's click on
//!   the HDR page; its label says it stays after Recall exits.
//! - The startup journal repair only restores what an older run changed, so it needs none.
//! - `recall-nvidia` reads the driver profiles (DRS) only and changes nothing.
//!
//! "Acknowledged" is per machine: `%LOCALAPPDATA%\Recall\display-changes-acknowledged`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Who asked for the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The user's own switch in the app: its label is the notice.
    UserAction,
    /// Config at start-up or a hand edit: no one saw an explanation yet.
    Automatic,
}

/// What to do before the first display change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Already acknowledged on this machine.
    Proceed,
    /// The user's action is the acknowledgement: remember it, then proceed.
    RememberAndProceed,
    /// Change nothing until the notice has been shown.
    NoticeFirst,
}

pub fn decide(acknowledged: bool, origin: Origin) -> Step {
    match (acknowledged, origin) {
        (true, _) => Step::Proceed,
        (false, Origin::UserAction) => Step::RememberAndProceed,
        (false, Origin::Automatic) => Step::NoticeFirst,
    }
}

/// How long after the notice became due it is shown: the island and app connect first.
pub const SETTLE: Duration = Duration::from_secs(3);

/// Show the pending notice now? Only when someone can see it (a connected client).
pub fn notice_ready(due_since: Instant, now: Instant, viewers: usize) -> bool {
    viewers > 0 && now.saturating_duration_since(due_since) >= SETTLE
}

pub const TITLE: &str = "Recall will manage HDR on this display";

/// The notice text for the managed display.
pub fn detail(display: &str) -> String {
    format!(
        "To record HDR games, Recall now changes Windows settings on {display}: HDR on or off, \
         SDR content brightness, and the NVIDIA colour clamp. Each is put back when Recall \
         exits. Turn it off on the Display & HDR page."
    )
}

/// The per-machine marker file.
pub fn ack_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Recall").join("display-changes-acknowledged"))
}

pub fn acknowledged(file: &Path) -> bool {
    file.is_file()
}

pub fn remember(file: &Path) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(file, b"Recall showed what it changes on this display and that it is put back on exit.\n")
        .map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unacknowledged_automatic_start_waits_for_the_notice() {
        assert_eq!(decide(true, Origin::Automatic), Step::Proceed);
        assert_eq!(decide(true, Origin::UserAction), Step::Proceed);
        assert_eq!(decide(false, Origin::UserAction), Step::RememberAndProceed);
        assert_eq!(decide(false, Origin::Automatic), Step::NoticeFirst);
    }

    #[test]
    fn the_notice_waits_for_a_viewer_and_the_settle_time() {
        let t0 = Instant::now();
        assert!(!notice_ready(t0, t0 + SETTLE, 0), "no client: nobody would see it");
        assert!(!notice_ready(t0, t0 + Duration::from_secs(1), 2), "island and app still connecting");
        assert!(notice_ready(t0, t0 + SETTLE, 1));
    }

    #[test]
    fn the_acknowledgement_persists_in_its_file() {
        let dir = std::env::temp_dir().join(format!("recall-notice-test-{}", std::process::id()));
        let file = dir.join("Recall").join("display-changes-acknowledged");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!acknowledged(&file));
        remember(&file).unwrap();
        assert!(acknowledged(&file));
        assert_eq!(decide(acknowledged(&file), Origin::Automatic), Step::Proceed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_notice_names_the_display_and_the_restore() {
        let text = detail("Test Monitor");
        assert!(text.contains("Test Monitor") && text.contains("put back when Recall exits"), "{text}");
    }
}

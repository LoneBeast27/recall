//! "Reinstall Recall": the installed version's own Setup, run through the same coupled update as
//! a new version (`update::hand_off`: resume file, `--update <dir> --resume <file>`, the
//! recorder saves first and comes back, the app reopens). Setup comes from the same pinned-host
//! WinHTTP path with the same checks as an update: the release for THIS version, not a draft or
//! a pre-release, its tag on main, its Setup asset's SHA-256 and size. Refused while a game is
//! being recorded or a long recording runs; with no such release, or no network, the answer is
//! the Releases page.
use std::path::Path;

use crate::update::{self, Release, Resume, net::Transport};

pub const RELEASES_URL: &str = "https://github.com/LoneBeast27/recall/releases";

/// The answer when Setup cannot be fetched here.
pub fn releases_page() -> String {
    format!("Download Setup from the Releases page: {RELEASES_URL}")
}

/// Fetch this version's Setup and hand the reinstall to it. On Ok the app quits itself next
/// (`UpdateUi::take_quit`'s path: flush, then quit). Blocking network: call it off the UI thread.
/// No button calls it yet (the repair entry point belongs to the Settings work that asked for it).
#[allow(dead_code)]
pub fn request_reinstall(net: &dyn Transport, install_dir: &Path, resume: &Resume) -> Result<(), String> {
    request_reinstall_with(net, update::current(), resume, |release| update::hand_off(net, release, install_dir, resume))
}

/// [`request_reinstall`] with the version and the hand-off given (tests).
pub(crate) fn request_reinstall_with(net: &dyn Transport, current: Option<&str>, resume: &Resume, hand_off: impl FnOnce(&Release) -> Result<(), String>) -> Result<(), String> {
    if resume.recorder.game {
        return Err("Recall is recording a game. Close the game, then reinstall.".into());
    }
    if resume.recorder.long_on {
        return Err("A long recording is running. Stop it, then reinstall.".into());
    }
    let current = current.ok_or_else(releases_page)?;
    let release = this_release(net, current)?;
    hand_off(&release)
}

/// The published release of `current`, checked as an update is.
fn this_release(net: &dyn Transport, current: &str) -> Result<Release, String> {
    let unreachable = |e: String| format!("{} ({e})", releases_page());
    let (status, body) = update::api(net, &format!("releases/tags/v{current}")).map_err(unreachable)?;
    if status != 200 {
        return Err(releases_page());
    }
    let r: update::ReleaseJson = serde_json::from_slice(&body).map_err(|e| unreachable(format!("Invalid GitHub release: {e}")))?;
    if r.draft || r.prerelease || r.tag_name.strip_prefix('v') != Some(current) {
        return Err(releases_page());
    }
    let release = update::release_of(r, current)?;
    let (status, body) = update::api(net, &format!("compare/v{current}...main?per_page=1&page=2")).map_err(unreachable)?;
    if status != 200 || !update::on_main(&body)? {
        return Err("The release tag could not be verified on main.".into());
    }
    Ok(release)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{RecorderState, net::fake::FakeNet};

    const TAG: &str = "https://api.github.com/repos/LoneBeast27/recall/releases/tags/v1.10.0";
    const COMPARE: &str = "https://api.github.com/repos/LoneBeast27/recall/compare/v1.10.0...main?per_page=1&page=2";

    fn release_json() -> String {
        serde_json::json!({
            "tag_name": "v1.10.0", "name": null, "body": null, "draft": false, "prerelease": false,
            "assets": [{"name": "RecallSetup.exe", "size": 3, "state": "uploaded",
                "digest": "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}]
        })
        .to_string()
    }

    fn idle() -> Resume {
        Resume::new("library", None, RecorderState { running: true, ..RecorderState::default() })
    }

    #[test]
    fn this_versions_checked_setup_is_handed_off() {
        let net = FakeNet::default().reply(TAG, 200, None, release_json().as_bytes()).reply(COMPARE, 200, None, br#"{"status":"ahead","behind_by":0}"#);
        let mut handed = None;
        request_reinstall_with(&net, Some("1.10.0"), &idle(), |r| {
            handed = Some(r.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(handed, Some(crate::update::tests::fixture_release()), "the same release and checks as an update");
    }

    #[test]
    fn a_game_or_a_long_recording_refuses_before_any_request() {
        let net = FakeNet::default();
        for recorder in [RecorderState { game: true, ..RecorderState::default() }, RecorderState { long_on: true, ..RecorderState::default() }] {
            let resume = Resume::new("library", None, recorder);
            assert!(request_reinstall_with(&net, Some("1.10.0"), &resume, |_| panic!("no hand-off")).is_err());
        }
        assert!(net.calls().is_empty());
    }

    #[test]
    fn no_release_no_main_or_no_network_points_at_the_releases_page() {
        let none = FakeNet::default(); // 404 for everything
        let e = request_reinstall_with(&none, Some("1.10.0"), &idle(), |_| panic!("no hand-off")).unwrap_err();
        assert_eq!(e, "Download Setup from the Releases page: https://github.com/LoneBeast27/recall/releases");
        let off_main = FakeNet::default().reply(TAG, 200, None, release_json().as_bytes()).reply(COMPARE, 200, None, br#"{"status":"diverged","behind_by":2}"#);
        assert!(request_reinstall_with(&off_main, Some("1.10.0"), &idle(), |_| panic!("no hand-off")).unwrap_err().contains("main"));
        let other = FakeNet::default().reply(TAG, 200, None, release_json().replace("v1.10.0", "v1.11.0").as_bytes());
        assert!(request_reinstall_with(&other, Some("1.10.0"), &idle(), |_| panic!("no hand-off")).unwrap_err().contains("Releases page"));
        assert!(request_reinstall_with(&none, None, &idle(), |_| panic!("no hand-off")).unwrap_err().contains(RELEASES_URL), "a dev build");
    }
}

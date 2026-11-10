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
/// (the update's path: flush, then quit). Blocking network: call it off the UI thread. The
/// recorder's old-engine notice calls it ([`reinstall_from_notice`]).
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

pub use crate::services::daemon::RepairNotice;

/// The action that shows the Reinstall button.
pub const REINSTALL: &str = "reinstall";
/// The button's label.
pub const BUTTON: &str = "Reinstall";
/// What the button says while Setup is fetched.
pub const RUNNING: &str = "Reinstalling: fetching this version's Setup…";

/// The notice to show with a Reinstall button, if the recorder asks for one.
pub fn reinstall_notice(repair: Option<&RepairNotice>) -> Option<&RepairNotice> {
    repair.filter(|r| r.action == REINSTALL)
}

/// A reinstall is being fetched / handed to Setup (one at a time).
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Its outcome, for the app's poll ([`take_outcome`]): Ok = Setup has it (the app quits).
static OUTCOME: std::sync::Mutex<Option<Result<(), String>>> = std::sync::Mutex::new(None);

/// The Reinstall button of the recorder's old-engine notice (R1; Rohan 2026-11-10 22:00 UTC,
/// verbatim: "for half finished installs, ideal if we were to make user patch it with clicking a
/// reinstall button,"): [`request_reinstall`] with what the update's install uses (the WinHTTP
/// transport, `update::install_dir`, the app's resume state), off the UI thread. The text the
/// click shows at once. Never runs Setup in a test or an offscreen build.
pub fn reinstall_from_notice(resume: Resume) -> String {
    if crate::TESTING || crate::offscreen::enabled() {
        return "Reinstall is off in this build.".to_owned();
    }
    start_reinstall(move || {
        let dir = update::install_dir()?;
        request_reinstall(&update::net::WinHttp, &dir, &resume)
    })
}

/// Run `job` (the reinstall) on its own thread unless one runs; the text to show now.
pub(crate) fn start_reinstall(job: impl FnOnce() -> Result<(), String> + Send + 'static) -> String {
    use std::sync::atomic::Ordering;
    if BUSY.swap(true, Ordering::SeqCst) {
        return RUNNING.to_owned();
    }
    std::thread::spawn(move || {
        let outcome = job();
        *OUTCOME.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
        BUSY.store(false, Ordering::SeqCst);
    });
    RUNNING.to_owned()
}

/// A reinstall is running (the button shows it).
pub fn reinstalling() -> bool {
    BUSY.load(std::sync::atomic::Ordering::SeqCst)
}

/// The finished reinstall's outcome, once (`root_poll`: Ok quits the app, Err is shown).
pub fn take_outcome() -> Option<Result<(), String>> {
    OUTCOME.lock().unwrap_or_else(|p| p.into_inner()).take()
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

    /// The button's click runs the reinstall off the UI thread (a fake job here: no Setup runs
    /// in a test), one at a time, and its outcome reaches the app's poll; the real click goes
    /// through `request_reinstall` with the update's transport, install dir and resume state.
    #[test]
    fn the_click_goes_through_request_reinstall_and_shows_its_outcome() {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        assert_eq!(start_reinstall(move || {
            let _ = wait.recv();
            Err("Download Setup from the Releases page: x".to_owned())
        }), RUNNING);
        assert!(reinstalling());
        assert_eq!(start_reinstall(|| panic!("a second one never runs")), RUNNING, "one at a time");
        go.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let outcome = loop {
            if let Some(o) = take_outcome() {
                break o;
            }
            assert!(std::time::Instant::now() < deadline, "no outcome");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(outcome, Err("Download Setup from the Releases page: x".to_owned()), "an error the user can read");
        let me = include_str!("repair.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(me.contains("let dir = update::install_dir()?; request_reinstall(&update::net::WinHttp, &dir, &resume)"), "the real transport and install dir");
        assert!(me.contains("if crate::TESTING || crate::offscreen::enabled() {"), "never Setup from a test build");
        let banner = include_str!("../views/repair_banner.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(banner.contains("ui::button(\"reinstall-recall\", Variant::Primary)"), "the button is on the notice");
        assert!(banner.contains("repair::reinstall_from_notice(this.update_resume())"), "its click calls it with the resume state");
        let poll = include_str!("../root_poll.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(poll.contains("crate::repair::take_outcome()"), "the app shows the outcome");
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

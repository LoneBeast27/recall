//! "Reinstall Recall": the installed version's own Setup, run through the same coupled update as
//! a new version (`update::hand_off`: resume file, `--update <dir> --resume <file>`, the
//! recorder saves first and comes back, the app reopens). Setup comes only from a verified
//! signed manifest, as an update does (`update::this_version`): the newest manifest across the
//! tiers must name THIS version, and Setup must match its signed size and SHA-256. Nothing
//! unsigned is ever run. Refused while a game is being recorded or a long recording runs; with
//! no verified manifest for this version, or no network, the answer is the Releases page.
use std::path::Path;

use crate::update::{self, Release, Resume, net::Transport, sources::Sources};

pub const RELEASES_URL: &str = "https://github.com/LoneBeast27/recall/releases";

/// The answer when Setup cannot be fetched here.
pub fn releases_page() -> String {
    format!("Download Setup from the Releases page: {RELEASES_URL}")
}

/// Fetch this version's Setup and hand the reinstall to it. On Ok the app quits itself next
/// (the update's path: flush, then quit). Blocking network: call it off the UI thread. The
/// recorder's old-engine notice calls it ([`reinstall_from_notice`]).
pub fn request_reinstall(net: &dyn Transport, install_dir: &Path, resume: &Resume) -> Result<(), String> {
    request_reinstall_with(net, update::current(), resume, &Sources::pinned(), |release| update::hand_off(net, release, install_dir, resume))
}

/// [`request_reinstall`] with the version and the hand-off given (tests).
pub(crate) fn request_reinstall_with(net: &dyn Transport, current: Option<&str>, resume: &Resume, sources: &Sources, hand_off: impl FnOnce(&Release) -> Result<(), String>) -> Result<(), String> {
    if resume.recorder.game {
        return Err("Recall is recording a game. Close the game, then reinstall.".into());
    }
    if resume.recorder.long_on {
        return Err("A long recording is running. Stop it, then reinstall.".into());
    }
    let current = current.ok_or_else(releases_page)?;
    let release = update::this_version(net, current, update::hotfix::revision(), sources).map_err(|e| {
        // The reason goes to the log; the screen gets a plain sentence (`update_words`).
        eprintln!("repair: reinstall refused: {e}");
        if e == update::NOT_THIS_VERSION { NOT_AVAILABLE.to_owned() } else { update::words::failure(&e).to_owned() }
    })?;
    hand_off(&release)
}

/// What the screen says when the signed update names another build than this one (a newer
/// release, or a host that has not caught up yet).
pub const NOT_AVAILABLE: &str = "This version's installer isn't available right now. Your version keeps working. Try again later.";

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

/// The Reinstall button of the recorder's old-engine notice (R1; the maintainer, 2026-11-10,
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
        crate::services::ui_wake::wake();
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

    use crate::update::{
        manifest::Role,
        signed_tests::{GITLAB, PAGES, REV_A, SETUP_URL, Signer, manifest, publish, sources},
    };

    fn idle() -> Resume {
        Resume::new("library", None, RecorderState { running: true, ..RecorderState::default() })
    }

    fn key() -> Signer {
        Signer::new(Role::Release, "release-1")
    }

    #[test]
    fn this_versions_signed_setup_is_handed_off() {
        let k = key();
        // The signed build is this one: a release build is stamped with its commit (the tag run
        // sets RECALL_SOURCE_REVISION), a plain test build is not and takes any signed build.
        let net = publish(FakeNet::default(), GITLAB, &manifest(202611210010, "1.10.0", update::hotfix::revision().unwrap_or(REV_A)), &k);
        let mut handed = None;
        request_reinstall_with(&net, Some("1.10.0"), &idle(), &sources(&[&k], &[GITLAB], None), |r| {
            handed = Some(r.clone());
            Ok(())
        })
        .unwrap();
        let handed = handed.unwrap();
        assert_eq!((handed.version.as_str(), handed.size, handed.urls.as_slice()), ("1.10.0", 3, [SETUP_URL.to_owned()].as_slice()));
        assert_eq!(handed.digest, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "the signed checksum");
    }

    #[test]
    fn a_game_or_a_long_recording_refuses_before_any_request() {
        let net = FakeNet::default();
        let s = sources(&[&key()], &[GITLAB], None);
        for recorder in [RecorderState { game: true, ..RecorderState::default() }, RecorderState { long_on: true, ..RecorderState::default() }] {
            let resume = Resume::new("library", None, recorder);
            assert!(request_reinstall_with(&net, Some("1.10.0"), &resume, &s, |_| panic!("no hand-off")).is_err());
        }
        assert!(net.calls().is_empty());
    }

    #[test]
    fn nothing_unsigned_is_ever_run() {
        let k = key();
        let s = sources(&[&k], &[GITLAB, PAGES], None);
        // Only the old GitHub release API answers (the account owner could change it): refused.
        let api_only = FakeNet::default().reply(
            "https://api.github.com/repos/LoneBeast27/recall/releases/tags/v1.10.0",
            200,
            None,
            br#"{"tag_name":"v1.10.0","draft":false,"prerelease":false,"assets":[]}"#,
        );
        let e = request_reinstall_with(&api_only, Some("1.10.0"), &idle(), &s, |_| panic!("no hand-off")).unwrap_err();
        assert_eq!(e, crate::update::words::COULDNT_CHECK, "a plain sentence, never the raw reason");
        assert!(api_only.calls().iter().all(|c| !c.contains("api.github.com")), "the API is never asked: {:?}", api_only.calls());
        // A stranger's signature: refused.
        let stranger = Signer::new(Role::Release, "release-1");
        let forged = publish(FakeNet::default(), GITLAB, &manifest(202611210010, "1.10.0", REV_A), &stranger);
        assert!(request_reinstall_with(&forged, Some("1.10.0"), &idle(), &s, |_| panic!("no hand-off")).is_err());
        // A newer signed version: install the update instead.
        let newer = publish(FakeNet::default(), GITLAB, &manifest(202611210010, "1.11.0", REV_A), &k);
        assert_eq!(request_reinstall_with(&newer, Some("1.10.0"), &idle(), &s, |_| panic!("no hand-off")).unwrap_err(), NOT_AVAILABLE);
        assert!(request_reinstall_with(&api_only, None, &idle(), &s, |_| panic!("no hand-off")).unwrap_err().contains(RELEASES_URL), "a dev build");
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

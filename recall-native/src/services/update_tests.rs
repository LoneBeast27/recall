//! The updater: offers, ancestry, automatic checks, and the confirm step before any download.
use super::{
    manifest::Role,
    net::fake::FakeNet,
    signed_tests::{GITLAB, Signer, manifest, publish, seq, sources},
    *,
};

pub(crate) fn fixture() -> serde_json::Value {
    serde_json::json!({
        "tag_name": "v1.10.0", "name": null, "body": null, "draft": false, "prerelease": false,
        "assets": [{"name": SETUP, "size": 3, "state": "uploaded",
            "digest": "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}]
    })
}

#[test]
fn newer_stable_versions_with_verified_assets_are_offered() {
    let read = |v: serde_json::Value, current| offer(v.to_string().as_bytes(), current);
    assert_eq!(read(fixture(), "1.9.0").unwrap().unwrap().version, "1.10.0");
    assert_eq!(read(fixture(), "1.10.0").unwrap(), None);
    for key in ["draft", "prerelease"] {
        let mut bad = fixture();
        bad[key] = true.into();
        assert_eq!(read(bad, "1.9.0").unwrap(), None);
    }
    let mut bad = fixture();
    bad["tag_name"] = "v1.10.0-rc1".into();
    assert_eq!(read(bad, "1.9.0").unwrap(), None);
    let mut bad = fixture();
    bad["assets"][0]["digest"] = serde_json::Value::Null;
    assert!(read(bad, "1.9.0").is_err());
}

/// The fixture's release: Setup is the 3 bytes "abc".
pub(crate) fn fixture_release() -> Release {
    offer(fixture().to_string().as_bytes(), "1.9.0").unwrap().unwrap()
}


/// An updater on a fake network, armed like a versioned on-screen build, trusting `KEY` on
/// the GitLab tier only.
fn ui(net: &Arc<FakeNet>, auto: bool) -> UpdateUi {
    let mut u = UpdateUi::with(net.clone(), Some("1.9.0"), auto, true, None);
    u.sources = Arc::new(sources(&[key()], &[GITLAB], None));
    u
}

fn key() -> &'static Signer {
    static KEY: std::sync::OnceLock<Signer> = std::sync::OnceLock::new();
    KEY.get_or_init(|| Signer::new(Role::Release, "release-1"))
}

/// Tick until the check thread answers (or 5 s pass).
fn settle(ui: &mut UpdateUi) {
    let end = Instant::now() + Duration::from_secs(5);
    while ui.busy() && Instant::now() < end {
        ui.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_bar_wakes_and_redraws_only_while_recall_is_in_front() {
    let net = Arc::new(FakeNet::default());
    let mut ui = ui(&net, false);
    assert_eq!(ui.next_frame(), None, "no bar, no frames");
    assert!(!ui.tick());
    ui.progress = Some(progress::Progress::check(progress::timings()));
    assert!(ui.next_frame().is_some() && ui.tick(), "in front: about 30 frames a second");
    ui.set_front(false);
    for _ in 0..3 {
        assert_eq!(ui.next_frame(), None, "behind or minimised: no wake for the bar");
        assert!(!ui.tick(), "and no redraw");
    }
    ui.set_front(true);
    assert!(ui.next_frame().is_some() && ui.tick(), "back in front: animates again");
    assert!(ui.progress_view().is_some(), "the bar kept its state while behind");
}

#[test]
fn automatic_checks_off_send_nothing_until_check_now() {
    let older = manifest(seq(10), "1.8.0", "1111111111111111111111111111111111111111");
    let net = Arc::new(publish(FakeNet::default(), GITLAB, &older, key()));
    let mut off = ui(&net, false);
    assert_eq!(off.check_at, None, "nothing is scheduled");
    for _ in 0..3 {
        assert!(!off.tick());
    }
    assert!(net.calls().is_empty(), "off sends no request: {:?}", net.calls());
    off.start_check(true); // what "Check now" runs
    settle(&mut off);
    assert_eq!(net.calls(), [GITLAB.to_owned(), format!("{GITLAB}.minisig")], "Check now always works");
    assert_eq!(off.notice, Some((true, "You're up to date.".into())), "only a verified manifest says so");
}

#[test]
fn automatic_checks_on_run_once_due_and_turning_off_cancels() {
    let net = Arc::new(FakeNet::default());
    let mut on = ui(&net, true);
    assert!(on.check_at.is_some(), "on: a check is scheduled after start-up");
    on.check_at = Some(Instant::now());
    assert!(on.tick());
    settle(&mut on);
    assert_eq!(net.calls(), [GITLAB]);
    assert_eq!(on.notice, None, "a failed automatic check stays quiet");
    let mut cancelled = ui(&net, true);
    cancelled.set_auto(false);
    assert_eq!(cancelled.check_at, None);
    assert!(!cancelled.auto);
    assert!(UpdateUi::with(net.clone(), None, true, true, None).check_at.is_none(), "dev builds never check");
    assert!(UpdateUi::with(net.clone(), Some("1.9.0"), true, false, None).check_at.is_none(), "tests and offscreen never check");
}

/// An updater showing the fixture's offer, on a fake network that records every request.
fn offered(net: &Arc<FakeNet>) -> UpdateUi {
    let mut u = ui(net, false);
    u.phase = Phase::Offered(fixture_release());
    u
}

/// The real hand-off's first request, on the fake network: the Setup download.
fn download_job(net: &Arc<FakeNet>) -> impl FnOnce(&Release) -> Result<(), String> + Send + 'static {
    let net = net.clone();
    move |release| {
        let url = format!("https://github.com/{REPO}/releases/download/v{}/{SETUP}", release.version);
        let mut sink = Vec::new();
        match net::fetch(net.as_ref(), &url, release.size, Duration::from_secs(5), &mut sink)? {
            200 => Ok(()),
            status => Err(format!("HTTP {status}")),
        }
    }
}

#[test]
fn a_dismissed_offer_or_a_cancelled_confirm_downloads_nothing() {
    let net = Arc::new(FakeNet::default());
    let mut later = offered(&net);
    later.later();
    later.install_with(download_job(&net));
    assert_eq!(later.phase, Phase::Idle);
    let mut cancelled = offered(&net);
    cancelled.confirm();
    assert_eq!(cancelled.phase, Phase::Confirm(fixture_release()), "the confirm step comes first");
    cancelled.cancel_confirm();
    assert_eq!(cancelled.phase, Phase::Offered(fixture_release()), "cancel goes back to the offer");
    cancelled.install_with(download_job(&net));
    let mut straight = offered(&net);
    straight.install_with(download_job(&net));
    assert_eq!(straight.phase, Phase::Offered(fixture_release()), "no download without the confirm step");
    std::thread::sleep(Duration::from_millis(50));
    assert!(net.calls().is_empty(), "nothing was requested: {:?}", net.calls());
    // The instrument works: "Save and update" does request the download.
    let mut confirmed = offered(&net);
    confirmed.confirm();
    confirmed.install_with(download_job(&net));
    assert!(matches!(confirmed.phase, Phase::Downloading(_)));
    settle(&mut confirmed);
    assert_eq!(net.calls(), ["https://github.com/LoneBeast27/recall/releases/download/v1.10.0/RecallSetup.exe"]);
    assert!(matches!(confirmed.phase, Phase::Failed(..)), "the fake network has no Setup: {:?}", confirmed.phase);
    assert!(!confirmed.take_quit(), "a failed hand-off never quits the app");
}

#[test]
fn a_hand_off_that_reached_setup_quits_the_app_once() {
    let net = Arc::new(FakeNet::default());
    let mut u = offered(&net);
    u.confirm();
    u.install_with(|_| Ok(()));
    settle(&mut u);
    assert_eq!(u.phase, Phase::Idle);
    assert!(u.take_quit());
    assert!(!u.take_quit(), "once");
    assert_eq!(CONFIRM_TEXT, "Updating closes Recall. A recording you started stops and is saved first. The replay buffer is not saved. Then Recall reopens.");
}

//! The updater: offers, ancestry, automatic checks, and the confirm step before any download.
use super::{net::fake::FakeNet, *};

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

#[test]
fn only_ancestors_of_main_pass_the_comparison() {
    assert!(on_main(br#"{"status":"ahead","behind_by":0}"#).unwrap());
    assert!(on_main(br#"{"status":"identical","behind_by":0}"#).unwrap());
    assert!(!on_main(br#"{"status":"diverged","behind_by":1}"#).unwrap());
    assert!(!on_main(br#"{"status":"behind","behind_by":1}"#).unwrap());
}

/// The fixture's release: Setup is the 3 bytes "abc".
pub(crate) fn fixture_release() -> Release {
    offer(fixture().to_string().as_bytes(), "1.9.0").unwrap().unwrap()
}

pub(crate) const LATEST: &str = "https://api.github.com/repos/LoneBeast27/recall/releases/latest";
pub(crate) const SUMS: &str = "https://github.com/LoneBeast27/recall/releases/download/v1.10.0/SHA256SUMS.txt";
pub(crate) const SUMS_BODY: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  RecallSetup.exe\r\n";

/// An updater on a fake network, armed like a versioned on-screen build.
fn ui(net: &Arc<FakeNet>, auto: bool) -> UpdateUi {
    UpdateUi::with(net.clone(), Some("1.9.0"), auto, true, None)
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
fn automatic_checks_off_send_nothing_until_check_now() {
    let net = Arc::new(FakeNet::default());
    let mut off = ui(&net, false);
    assert_eq!(off.check_at, None, "nothing is scheduled");
    for _ in 0..3 {
        assert!(!off.tick());
    }
    assert!(net.calls().is_empty(), "off sends no request: {:?}", net.calls());
    off.start_check(true); // what "Check now" runs
    settle(&mut off);
    assert_eq!(net.calls(), [LATEST], "Check now always works");
    assert_eq!(off.notice, Some((true, "No newer stable release is available.".into())));
}

#[test]
fn automatic_checks_on_run_once_due_and_turning_off_cancels() {
    let net = Arc::new(FakeNet::default());
    let mut on = ui(&net, true);
    assert!(on.check_at.is_some(), "on: a check is scheduled after start-up");
    on.check_at = Some(Instant::now());
    assert!(on.tick());
    settle(&mut on);
    assert_eq!(net.calls(), [LATEST]);
    let mut cancelled = ui(&net, true);
    cancelled.set_auto(false);
    assert_eq!(cancelled.check_at, None);
    assert!(!cancelled.auto);
    assert!(UpdateUi::with(net.clone(), None, true, true, None).check_at.is_none(), "dev builds never check");
    assert!(UpdateUi::with(net.clone(), Some("1.9.0"), true, false, None).check_at.is_none(), "tests and offscreen never check");
}

#[test]
fn a_newer_release_on_main_is_offered_and_an_older_one_is_not() {
    let compare = "https://api.github.com/repos/LoneBeast27/recall/compare/v1.10.0...main?per_page=1&page=2";
    let net = FakeNet::default()
        .reply(LATEST, 200, None, fixture().to_string().as_bytes())
        .reply(compare, 200, None, br#"{"status":"ahead","behind_by":0}"#)
        .reply(SUMS, 200, None, SUMS_BODY.as_bytes());
    assert_eq!(check(&net, "1.9.0", None, true).unwrap(), Some(fixture_release()));
    assert_eq!(check(&net, "1.10.0", None, true).unwrap(), None, "same version");
    assert_eq!(check(&net, "2.0.0", None, true).unwrap(), None, "never a downgrade");
    let off_main = FakeNet::default()
        .reply(LATEST, 200, None, fixture().to_string().as_bytes())
        .reply(compare, 200, None, br#"{"status":"diverged","behind_by":1}"#)
        .reply(SUMS, 200, None, SUMS_BODY.as_bytes());
    assert!(check(&off_main, "1.9.0", None, true).is_err());
    let no_sums = FakeNet::default()
        .reply(LATEST, 200, None, fixture().to_string().as_bytes())
        .reply(compare, 200, None, br#"{"status":"ahead","behind_by":0}"#);
    assert!(check(&no_sums, "1.9.0", None, true).is_err(), "a newer version needs SHA256SUMS.txt too");
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
    assert_eq!(CONFIRM_TEXT, "Updating closes Recall. A recording in progress stops and is saved, with the replay buffer, then Recall reopens.");
}

//! Same-version hotfixes: offered once at start-up, never for the same build, never backwards,
//! never without a revision stamp, never unverified, and "Skip" is per build.
use std::{io::Write, time::Instant};

use super::{
    hotfix::{parse_revision, skip_key, stamped, sums_match},
    net::{self, Reply, fake::FakeNet},
    tests::{LATEST, SUMS, SUMS_BODY, fixture},
    *,
};

/// The build already installed (today's 1.10.0) and the hotfix that replaced its Setup.
const OLD: &str = "1111111111111111111111111111111111111111";
const FIX: &str = "2222222222222222222222222222222222222222";
const API: &str = "https://api.github.com/repos/LoneBeast27/recall/";
const REVISION: &str = "https://github.com/LoneBeast27/recall/releases/download/v1.10.0/SOURCE-REVISION.txt";

fn compare(base: &str, head: &str) -> String {
    format!("{API}compare/{base}...{head}?per_page=1&page=2")
}

/// GitHub after the hotfix: the v1.10.0 tag moved to `FIX` on main, its assets re-uploaded.
fn published(fix_vs_old: &str) -> FakeNet {
    FakeNet::default()
        .reply(LATEST, 200, None, fixture().to_string().as_bytes())
        .reply(&compare("v1.10.0", "main"), 200, None, br#"{"status":"ahead","behind_by":0}"#)
        .reply(REVISION, 200, None, format!("{FIX}\r\n").as_bytes())
        .reply(&compare("v1.10.0", FIX), 200, None, br#"{"status":"identical","behind_by":0}"#)
        .reply(&compare(OLD, FIX), 200, None, fix_vs_old.as_bytes())
        .reply(SUMS, 200, None, SUMS_BODY.as_bytes())
}

const AHEAD: &str = r#"{"status":"ahead","behind_by":0}"#;

#[test]
fn a_hotfix_is_offered_once_at_start_up_then_never_again() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let net = published(AHEAD);
    // Today's build, automatic start-up check (not manual): one offer, as a hotfix.
    let offer = check(&net, "1.10.0", Some(OLD), false).unwrap().expect("the hotfix is offered");
    assert_eq!(offer.hotfix.as_deref(), Some(FIX));
    assert_eq!(offer.title, "Hotfix for 1.10.0");
    assert_eq!(offer.version, "1.10.0", "the version number stays");
    // After the update the app is the hotfix build: same revision, nothing offered, twice.
    for _ in 0..2 {
        assert_eq!(check(&net, "1.10.0", Some(FIX), false).unwrap(), None, "same build: no update loop");
    }
    // The hotfix build never asks for the tag comparisons again: it stops at its own revision.
    let calls = net.calls();
    let after = &calls[calls.iter().rposition(|c| c == LATEST).unwrap()..];
    assert_eq!(after, [LATEST.to_owned(), compare("v1.10.0", "main"), REVISION.to_owned()]);
}

#[test]
fn builds_without_a_revision_stamp_never_get_a_same_version_offer() {
    let net = published(AHEAD);
    assert_eq!(check(&net, "1.10.0", None, false).unwrap(), None);
    assert_eq!(net.calls(), [LATEST], "nothing past the latest release is asked");
    assert_eq!(stamped(None), None);
    assert_eq!(stamped(Some("")), None, "an empty stamp");
    assert_eq!(stamped(Some("abc123")), None, "a short revision");
    assert_eq!(stamped(Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01")), None, "only git's lowercase form");
    assert_eq!(stamped(Some(OLD)), Some(OLD));
}

#[test]
fn an_older_or_unrelated_build_is_never_offered() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    for answer in [r#"{"status":"behind","behind_by":3}"#, r#"{"status":"diverged","behind_by":1}"#] {
        assert_eq!(check(&published(answer), "1.10.0", Some(OLD), false).unwrap(), None, "{answer}");
    }
    // This build's commit unknown to GitHub (a local build): 404, nothing offered.
    let unknown = published(AHEAD);
    let unknown = FakeNet { replies: { let mut r = unknown.replies; r.remove(&compare(OLD, FIX)); r }, ..Default::default() };
    assert_eq!(check(&unknown, "1.10.0", Some(OLD), false).unwrap(), None);
}

#[test]
fn an_unverified_hotfix_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let tampered = published(AHEAD).reply(SUMS, 200, None, format!("{}  RecallSetup.exe", "0".repeat(64)).as_bytes());
    assert!(check(&tampered, "1.10.0", Some(OLD), true).is_err(), "SHA256SUMS.txt must list GitHub's checksum");
    let no_sums = published(AHEAD).reply(SUMS, 404, None, b"");
    assert!(check(&no_sums, "1.10.0", Some(OLD), true).is_err());
    let not_the_tag = published(AHEAD).reply(&compare("v1.10.0", FIX), 200, None, AHEAD.as_bytes());
    assert!(check(&not_the_tag, "1.10.0", Some(OLD), true).is_err(), "the revision must be the tag's commit");
    let bad_revision = published(AHEAD).reply(REVISION, 200, None, b"not a commit");
    assert!(check(&bad_revision, "1.10.0", Some(OLD), true).is_err());
    let off_main = published(AHEAD).reply(&compare("v1.10.0", "main"), 200, None, br#"{"status":"diverged","behind_by":1}"#);
    assert!(check(&off_main, "1.10.0", Some(OLD), true).is_err(), "the tag must be on main");
}

#[test]
fn skip_is_per_build() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let net = published(AHEAD);
    let offer = check(&net, "1.10.0", Some(OLD), false).unwrap().unwrap();
    assert_eq!(skip_key(&offer), format!("1.10.0+{FIX}"));
    skip(&skip_key(&offer));
    assert_eq!(check(&net, "1.10.0", Some(OLD), false).unwrap(), None, "skipped: not at start-up");
    assert!(check(&net, "1.10.0", Some(OLD), true).unwrap().is_some(), "Check now still finds it");
    // Skipping plain 1.10.0 (an offer made before the hotfix) does not hide the hotfix.
    skip("1.10.0");
    assert!(check(&net, "1.10.0", Some(OLD), false).unwrap().is_some());
}

/// A network that is not there.
struct Offline;

impl net::Transport for Offline {
    fn get(&self, _: &str, _: &str, _: u64, _: Instant, _: &mut dyn Write) -> Result<Reply, String> {
        Err("No connection.".into())
    }
}

#[test]
fn offline_at_start_up_shows_nothing() {
    let mut ui = UpdateUi::with(Arc::new(Offline), Some("1.10.0"), true, true, None);
    ui.revision = Some(OLD);
    ui.check_at = Some(Instant::now());
    assert!(ui.tick());
    let end = Instant::now() + Duration::from_secs(5);
    while ui.busy() && Instant::now() < end {
        ui.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!ui.busy());
    assert_eq!(ui.phase, Phase::Idle);
    assert_eq!(ui.notice, None, "no prompt and no notice");
}

#[test]
fn release_files_parse_strictly() {
    assert_eq!(parse_revision(&format!("\u{feff}{FIX}\r\n")).as_deref(), Some(FIX));
    assert_eq!(parse_revision(&FIX.to_uppercase()).as_deref(), Some(FIX));
    assert_eq!(parse_revision("abc"), None);
    assert_eq!(parse_revision(""), None);
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert!(sums_match(SUMS_BODY, digest));
    assert!(sums_match(&format!("{}  *RecallSetup.exe\n", digest.to_uppercase()), digest));
    assert!(!sums_match(&format!("{digest}  Other.exe"), digest), "the checksum of another file");
    assert!(!sums_match(digest, digest), "no file name");
}

/// Drive an armed updater's check to its answer.
fn run(ui: &mut UpdateUi, manual: bool) {
    ui.start_check(manual);
    let end = Instant::now() + Duration::from_secs(5);
    while ui.busy() && Instant::now() < end {
        ui.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_hotfix_that_did_not_become_this_build_is_not_offered_again_this_session() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let net = Arc::new(published(AHEAD));
    // The update ran, but the app reopened as the old build (Setup refused, or another build).
    let mut ui = UpdateUi::with(net.clone(), Some("1.10.0"), true, true, None);
    ui.revision = Some(OLD);
    ui.after_update(Some(FIX));
    run(&mut ui, false);
    assert_eq!(ui.phase, Phase::Idle, "no offer loop at start-up");
    run(&mut ui, true);
    assert!(matches!(&ui.phase, Phase::Offered(r) if r.hotfix.as_deref() == Some(FIX)), "Check now still shows it: {:?}", ui.phase);
    // The instrument: without the refusal the same start-up check offers it.
    let mut fresh = UpdateUi::with(net.clone(), Some("1.10.0"), true, true, None);
    fresh.revision = Some(OLD);
    fresh.after_update(None);
    run(&mut fresh, false);
    assert!(matches!(fresh.phase, Phase::Offered(_)));
    // The update worked: this build is the expected one, nothing is refused.
    let mut updated = UpdateUi::with(net, Some("1.10.0"), true, true, None);
    updated.revision = Some(FIX);
    updated.after_update(Some(FIX));
    assert_eq!(updated.refused, None);
}

#[test]
fn github_errors_on_the_hotfix_requests_never_offer() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let tag = compare("v1.10.0", FIX);
    let ancestry = compare(OLD, FIX);
    for url in [REVISION, tag.as_str(), ancestry.as_str(), SUMS] {
        for (status, body) in [(403, &br#"{"message":"rate limit"}"#[..]), (429, b""), (200, b"{not json")] {
            if url == REVISION && status == 200 {
                // Text, not JSON: covered by "not a commit" in an_unverified_hotfix_is_refused.
                continue;
            }
            let net = published(AHEAD).reply(url, status, None, body);
            let got = check(&net, "1.10.0", Some(OLD), false);
            assert!(!matches!(got, Ok(Some(_))), "{url} {status}: {got:?}");
        }
    }
    // The latest-release call itself.
    for (status, body) in [(403, &b""[..]), (429, b""), (200, b"{not json")] {
        let net = published(AHEAD).reply(LATEST, status, None, body);
        assert!(!matches!(check(&net, "1.10.0", Some(OLD), false), Ok(Some(_))), "latest {status}");
    }
}

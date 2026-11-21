//! Same-version hotfixes from the signed manifest: offered only to a build the hotfix names in
//! `supersedes`, never for the same build, never backwards, never without a revision stamp,
//! once at start-up after a refused install, and "Skip" is per build.
use std::{io::Write, time::Instant};

use serde_json::json;

use super::{
    hotfix::{skip_key, stamped},
    manifest::Role,
    net::{self, Reply, fake::FakeNet},
    signed_tests::{GITLAB, Signer, manifest, publish, seq, sources},
    *,
};

/// The build already installed (today's 1.10.0) and the hotfix that replaced its Setup.
const OLD: &str = "1111111111111111111111111111111111111111";
const FIX: &str = "2222222222222222222222222222222222222222";

/// The hotfix manifest: 1.10.0 built from `FIX`, replacing the builds in `supersedes`.
fn published(key: &Signer, supersedes: &[&str]) -> FakeNet {
    let mut m = manifest(seq(50), "1.10.0", FIX);
    m["supersedes"] = json!(supersedes);
    publish(FakeNet::default(), GITLAB, &m, key)
}

fn key() -> Signer {
    Signer::new(Role::Release, "release-1")
}

#[test]
fn a_hotfix_is_offered_only_to_a_build_it_replaces() {
    let k = key();
    let s = sources(&[&k], &[GITLAB], None);
    let net = published(&k, &[OLD]);
    let offer = check(&net, "1.10.0", Some(OLD), false, &s).unwrap().expect("the hotfix is offered");
    assert_eq!(offer.hotfix.as_deref(), Some(FIX));
    assert_eq!(offer.title, "A fix for Recall 1.10.0");
    assert_eq!(offer.version, "1.10.0", "the version number stays");
    for _ in 0..2 {
        assert_eq!(check(&net, "1.10.0", Some(FIX), false, &s).unwrap(), None, "same build: no update loop");
    }
    // A build the hotfix does not name (a newer build installed by hand): never offered back.
    let other = "3333333333333333333333333333333333333333";
    assert_eq!(check(&net, "1.10.0", Some(other), false, &s).unwrap(), None);
    // A hotfix that names no builds is offered to nobody on that version.
    assert_eq!(check(&published(&k, &[]), "1.10.0", Some(OLD), false, &s).unwrap(), None);
}

#[test]
fn builds_without_a_revision_stamp_never_get_a_same_version_offer() {
    let k = key();
    let s = sources(&[&k], &[GITLAB], None);
    assert_eq!(check(&published(&k, &[OLD]), "1.10.0", None, false, &s).unwrap(), None);
    assert_eq!(stamped(None), None);
    assert_eq!(stamped(Some("")), None, "an empty stamp");
    assert_eq!(stamped(Some("abc123")), None, "a short revision");
    assert_eq!(stamped(Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01")), None, "only git's lowercase form");
    assert_eq!(stamped(Some(OLD)), Some(OLD));
}

#[test]
fn skip_is_per_build() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let k = key();
    let s = sources(&[&k], &[GITLAB], None);
    let net = published(&k, &[OLD]);
    let offer = check(&net, "1.10.0", Some(OLD), false, &s).unwrap().unwrap();
    assert_eq!(skip_key(&offer), format!("1.10.0+{FIX}"));
    skip(&skip_key(&offer));
    assert_eq!(check(&net, "1.10.0", Some(OLD), false, &s).unwrap(), None, "skipped: not at start-up");
    assert!(check(&net, "1.10.0", Some(OLD), true, &s).unwrap().is_some(), "Check now still finds it");
    // Skipping plain 1.10.0 (an offer made before the hotfix) does not hide the hotfix.
    skip("1.10.0");
    assert!(check(&net, "1.10.0", Some(OLD), false, &s).unwrap().is_some());
}

/// A network that is not there.
struct Offline;

impl net::Transport for Offline {
    fn get(&self, _: &str, _: &str, _: u64, _: Instant, _: &mut dyn Write) -> Result<Reply, String> {
        Err("No connection.".into())
    }
}

/// An armed updater for today's build `OLD`, on `net`, trusting `k` on the GitLab tier only.
fn armed(net: Arc<dyn net::Transport>, k: &Signer) -> UpdateUi {
    let mut ui = UpdateUi::with(net, Some("1.10.0"), true, true, None);
    ui.sources = Arc::new(sources(&[k], &[GITLAB], None));
    ui.revision = Some(OLD);
    ui
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
fn offline_at_start_up_shows_nothing() {
    let mut ui = armed(Arc::new(Offline), &key());
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
fn a_hotfix_that_did_not_become_this_build_is_not_offered_again_this_session() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let k = key();
    let net: Arc<dyn net::Transport> = Arc::new(published(&k, &[OLD]));
    // The update ran, but the app reopened as the old build (Setup refused, or another build).
    let mut ui = armed(net.clone(), &k);
    ui.after_update(Some(FIX));
    run(&mut ui, false);
    assert_eq!(ui.phase, Phase::Idle, "no offer loop at start-up");
    run(&mut ui, true);
    assert!(matches!(&ui.phase, Phase::Offered(r) if r.hotfix.as_deref() == Some(FIX)), "Check now still shows it: {:?}", ui.phase);
    // The instrument: without the refusal the same start-up check offers it.
    let mut fresh = armed(net.clone(), &k);
    fresh.after_update(None);
    fresh.refused = None; // a profile without refused.txt
    run(&mut fresh, false);
    assert!(matches!(fresh.phase, Phase::Offered(_)));
    // The update worked: this build is the expected one, nothing new is refused.
    let mut updated = armed(net, &k);
    updated.revision = Some(FIX);
    updated.refused = None;
    updated.after_update(Some(FIX));
    assert_eq!(updated.refused, None);
}

#[test]
fn a_refused_hotfix_stays_refused_after_a_restart() {
    let root = tempfile::tempdir().unwrap();
    let _home = crate::user_dirs::inject(root.path());
    let k = key();
    let net: Arc<dyn net::Transport> = Arc::new(published(&k, &[OLD]));
    let mut first = armed(net.clone(), &k);
    first.after_update(Some(FIX));
    let file = root.path().join(r"AppData\Local\Recall\updates\refused.txt");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("1.10.0+{FIX}"), "kept on disk, version+commit");
    // The next start (no --resume): still not offered at start-up.
    let mut restarted = armed(net.clone(), &k);
    restarted.refused = hotfix::load_refused();
    assert_eq!(restarted.refused.as_deref(), Some(format!("1.10.0+{FIX}").as_str()));
    run(&mut restarted, false);
    assert_eq!(restarted.phase, Phase::Idle);
    run(&mut restarted, true);
    assert!(matches!(restarted.phase, Phase::Offered(_)), "Check now still shows it");
    // A later hotfix (another commit) is a different key: offered.
    let mut later = armed(net, &k);
    later.refused = Some(format!("1.10.0+{}", "3".repeat(40)));
    run(&mut later, false);
    assert!(matches!(later.phase, Phase::Offered(_)), "{:?}", later.phase);
}

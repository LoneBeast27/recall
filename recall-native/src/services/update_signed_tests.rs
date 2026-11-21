//! The signed manifest, the tiers and the trust file, on a fake network with throwaway keys
//! made per test run (never a real key). Setup is the 3 bytes "abc" throughout.
use std::path::Path;

use serde_json::{Value, json};

use super::{
    manifest::{BAD_SIGNATURE, PinnedKey, Role, parse_keys, verify},
    net::fake::FakeNet,
    sources::{NO_SOURCE, Sources, parse_tiers},
    trust::TrustState,
    *,
};

pub(crate) const GITLAB: &str = "https://gitlab.com/example/recall-updates/-/raw/main/recall-update.json";
pub(crate) const PAGES: &str = "https://updates.example.org/recall-update.json";
pub(crate) const GITHUB: &str = "https://github.com/LoneBeast27/recall/releases/latest/download/recall-update.json";
pub(crate) const SETUP_URL: &str = "https://gitlab.com/example/recall-updates/-/packages/generic/recall/1.10.0/RecallSetup.exe";
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
pub(crate) const REV_A: &str = "1111111111111111111111111111111111111111";
pub(crate) const REV_B: &str = "2222222222222222222222222222222222222222";

/// A throwaway minisign key pair standing in for a pinned key.
pub(crate) struct Signer {
    pair: minisign::KeyPair,
    pub role: Role,
    pub name: &'static str,
}

impl Signer {
    pub(crate) fn new(role: Role, name: &'static str) -> Self {
        Self { pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(), role, name }
    }
    pub(crate) fn pinned(&self) -> PinnedKey {
        PinnedKey::new(self.role, self.name, &self.pair.pk.to_base64()).unwrap()
    }
    pub(crate) fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        minisign::sign(Some(&self.pair.pk), &self.pair.sk, std::io::Cursor::new(bytes), Some("test"), None).unwrap().into_string().into_bytes()
    }
}

/// The `n`th minute of 2026-11-21 as a manifest sequence (`yyyymmddHHMM`).
pub(crate) fn seq(n: u64) -> u64 {
    202611210000 + n / 60 * 100 + n % 60
}

/// A manifest for `version` at `sequence`, built by `revision`.
pub(crate) fn manifest(sequence: u64, version: &str, revision: &str) -> Value {
    json!({
        "schema": 1, "channel": "stable", "sequence": sequence, "version": version, "revision": revision,
        "published": "2026-11-21T00:00:00Z", "title": null, "notes": "Fixes.",
        "setup": {"name": SETUP, "size": 3, "sha256": ABC, "urls": [SETUP_URL]},
    })
}

/// `net` with `json` (signed by `signer`) published at the tier `url`.
pub(crate) fn publish(net: FakeNet, url: &str, json: &Value, signer: &Signer) -> FakeNet {
    let bytes = json.to_string().into_bytes();
    let signature = signer.sign(&bytes);
    net.reply(url, 200, None, &bytes).reply(&format!("{url}.minisig"), 200, None, &signature)
}

pub(crate) fn sources(keys: &[&Signer], tiers: &[&str], trust_file: Option<&Path>) -> Sources {
    Sources { keys: keys.iter().map(|k| k.pinned()).collect(), tiers: tiers.iter().map(|t| t.to_string()).collect(), trust_file: trust_file.map(Path::to_path_buf) }
}

fn release_key() -> Signer {
    Signer::new(Role::Release, "release-1")
}

fn tiers3() -> [&'static str; 3] {
    [GITLAB, PAGES, GITHUB]
}

#[test]
fn a_verified_manifest_gives_the_signed_setup_and_every_tier_is_asked() {
    let key = release_key();
    let net = publish(FakeNet::default(), GITLAB, &manifest(seq(10), "1.10.0", REV_A), &key);
    let s = sources(&[&key], &tiers3(), None);
    let offer = check(&net, "1.9.0", None, false, &s).unwrap().unwrap();
    assert_eq!((offer.version.as_str(), offer.urls.as_slice(), offer.digest.as_str()), ("1.10.0", [SETUP_URL.to_owned()].as_slice(), ABC));
    for tier in tiers3() {
        assert!(net.calls().iter().any(|c| c == tier), "{tier} asked: {:?}", net.calls());
    }
}

#[test]
fn a_tier_that_is_missing_or_unreachable_falls_through_to_the_next() {
    let key = release_key();
    let m = manifest(seq(10), "1.10.0", REV_A);
    let s = sources(&[&key], &tiers3(), None);
    // GitLab 404, the domain's site good.
    let net = publish(FakeNet::default(), PAGES, &m, &key);
    assert!(check(&net, "1.9.0", None, false, &s).unwrap().is_some());
    // GitLab and the domain's site unreachable, GitHub good: each tier gets one try.
    let net = publish(FakeNet::default(), GITHUB, &m, &key)
        .fail_once(GITLAB, "Could not reach the update server: no route")
        .fail_once(PAGES, super::net::TIMED_OUT);
    assert!(check(&net, "1.9.0", None, false, &s).unwrap().is_some());
    assert_eq!(net.calls().iter().filter(|c| c.as_str() == GITLAB).count(), 1, "no retry on the same tier");
}

#[test]
fn no_verified_manifest_anywhere_is_couldnt_check_never_up_to_date() {
    let key = release_key();
    let s = sources(&[&key], &tiers3(), None);
    for manual in [false, true] {
        let got = check(&FakeNet::default(), "1.9.0", None, manual, &s).unwrap_err();
        assert!(got.starts_with(NO_SOURCE), "{got}");
        assert_eq!(words::failure(&got), words::COULDNT_CHECK);
    }
    // A build with no pinned keys refuses everything, signed or not (fail closed).
    let net = publish(FakeNet::default(), GITLAB, &manifest(seq(10), "1.10.0", REV_A), &key);
    let none = sources(&[], &tiers3(), None);
    let got = check(&net, "1.9.0", None, true, &none).unwrap_err();
    assert!(got.starts_with(NO_SOURCE) && got.contains("no pinned update keys"), "{got}");
}

#[test]
fn a_wrong_key_a_changed_byte_or_a_revoked_key_is_refused() {
    let key = release_key();
    let stranger = Signer::new(Role::Release, "release-1");
    let m = manifest(seq(10), "1.10.0", REV_A);
    let bytes = m.to_string().into_bytes();
    let good = key.sign(&bytes);
    let pinned = [key.pinned()];
    assert!(verify(&bytes, &good, &pinned, &[]).is_ok(), "the instrument: the right key verifies");
    assert_eq!(verify(&bytes, &stranger.sign(&bytes), &pinned, &[]).unwrap_err(), BAD_SIGNATURE);
    let mut changed = bytes.clone();
    let at = changed.iter().position(|b| *b == b'3').unwrap(); // the Setup size
    changed[at] = b'4';
    assert_eq!(verify(&changed, &good, &pinned, &[]).unwrap_err(), BAD_SIGNATURE);
    assert_eq!(verify(&bytes, &good, &pinned, &["release-1".into()]).unwrap_err(), BAD_SIGNATURE);
    assert_eq!(verify(&bytes, b"not a signature", &pinned, &[]).unwrap_err(), BAD_SIGNATURE);
    // A tier serving a stranger's file is skipped; the next tier's real one is used.
    let net = publish(publish(FakeNet::default(), GITLAB, &manifest(seq(99), "6.6.6", REV_A), &stranger), PAGES, &m, &key);
    let offer = check(&net, "1.9.0", None, false, &sources(&[&key], &tiers3(), None)).unwrap().unwrap();
    assert_eq!(offer.version, "1.10.0");
}

#[test]
fn fields_are_checked_after_the_signature() {
    let key = release_key();
    let pinned = [key.pinned()];
    let read = |m: Value| {
        let b = m.to_string().into_bytes();
        verify(&b, &key.sign(&b), &pinned, &[])
    };
    assert!(read(manifest(seq(1), "1.10.0", REV_A)).is_ok());
    let cases: [(&str, Value); 8] = [
        ("schema", json!(2)),
        ("channel", json!("beta")),
        ("version", json!("1.10")),
        ("revision", json!("abc")),
        ("min_version", json!("one")),
        ("setup", json!({"name": "Other.exe", "size": 3, "sha256": ABC, "urls": [SETUP_URL]})),
        ("setup", json!({"name": SETUP, "size": 0, "sha256": ABC, "urls": [SETUP_URL]})),
        ("setup", json!({"name": SETUP, "size": 3, "sha256": "xyz", "urls": [SETUP_URL]})),
    ];
    for (field, value) in cases {
        let mut m = manifest(seq(1), "1.10.0", REV_A);
        m[field] = value.clone();
        assert!(read(m).is_err(), "{field} = {value}");
    }
    // Only pinned https addresses are kept; none left is a refusal.
    let mut m = manifest(seq(1), "1.10.0", REV_A);
    m["setup"]["urls"] = json!(["http://gitlab.com/a", "https://downloads.example.net/RecallSetup.exe", SETUP_URL]);
    assert_eq!(read(m).unwrap().urls, [SETUP_URL]);
    let mut m = manifest(seq(1), "1.10.0", REV_A);
    m["setup"]["urls"] = json!(["https://downloads.example.net/RecallSetup.exe"]);
    assert!(read(m).is_err());
    // A later build's extra fields are ignored, not refused.
    let mut m = manifest(seq(1), "1.10.0", REV_A);
    m["future"] = json!({"x": 1});
    m["setup"]["future"] = json!(true);
    assert!(read(m).is_ok());
}

#[test]
fn never_a_downgrade_and_never_a_replayed_older_manifest() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("trust.json");
    let key = release_key();
    let s = sources(&[&key], &[GITLAB], Some(&file));
    let newer = publish(FakeNet::default(), GITLAB, &manifest(seq(300), "1.10.0", REV_A), &key);
    assert!(check(&newer, "1.9.0", None, true, &s).unwrap().is_some());
    assert_eq!(TrustState::load(Some(&file)).highest_sequence, seq(300), "remembered");
    assert_eq!(check(&newer, "2.0.0", None, true, &s).unwrap(), None, "an older version is never offered");
    // A host replays an older, validly signed manifest: refused, not "up to date".
    let replay = publish(FakeNet::default(), GITLAB, &manifest(seq(200), "1.9.5", REV_A), &key);
    let got = check(&replay, "1.9.0", None, true, &s).unwrap_err();
    assert!(got.starts_with(NO_SOURCE) && got.contains("older than one already accepted"), "{got}");
    // The same manifest again is fine.
    assert!(check(&newer, "1.9.0", None, true, &s).unwrap().is_some());
}

#[test]
fn every_check_takes_the_newest_so_one_frozen_tier_cannot_hide_a_release() {
    let key = release_key();
    let net = publish(
        publish(FakeNet::default(), GITLAB, &manifest(seq(100), "1.10.0", REV_A), &key),
        PAGES,
        &manifest(seq(200), "1.11.0", REV_B),
        &key,
    );
    let s = sources(&[&key], &tiers3(), None);
    for manual in [false, true] {
        assert_eq!(check(&net, "1.9.0", None, manual, &s).unwrap().unwrap().version, "1.11.0", "the first tier is frozen on 1.10.0");
    }
    // A build already on 1.10.0 is not told "up to date" by the frozen first tier.
    assert_eq!(check(&net, "1.10.0", None, false, &s).unwrap().unwrap().version, "1.11.0");
}

#[test]
fn only_the_backup_key_revokes_and_a_revoked_key_stays_refused() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("trust.json");
    let release = release_key();
    let backup = Signer::new(Role::Backup, "backup-1");
    let s = sources(&[&release, &backup], &[GITLAB], Some(&file));
    // The release key cannot revoke the backup key.
    let mut m = manifest(seq(10), "1.10.0", REV_A);
    m["revoke_keys"] = json!(["backup-1"]);
    check(&publish(FakeNet::default(), GITLAB, &m, &release), "1.9.0", None, true, &s).unwrap();
    assert!(TrustState::load(Some(&file)).revoked.is_empty());
    // The backup key revokes the release key (and never itself).
    let mut m = manifest(seq(20), "1.10.1", REV_A);
    m["revoke_keys"] = json!(["release-1", "backup-1"]);
    let offer = check(&publish(FakeNet::default(), GITLAB, &m, &backup), "1.9.0", None, true, &s).unwrap().unwrap();
    assert_eq!(offer.version, "1.10.1", "a backup-signed update is offered");
    assert_eq!(TrustState::load(Some(&file)).revoked, ["release-1"]);
    // From now on the release key's files are refused, even a newer one.
    let after = publish(FakeNet::default(), GITLAB, &manifest(seq(30), "1.10.2", REV_A), &release);
    assert!(check(&after, "1.9.0", None, true, &s).unwrap_err().starts_with(NO_SOURCE));
}

#[test]
fn a_build_below_min_version_is_told_to_reinstall() {
    let key = release_key();
    let mut m = manifest(seq(10), "1.10.0", REV_A);
    m["min_version"] = json!("1.5.0");
    let net = publish(FakeNet::default(), GITLAB, &m, &key);
    let s = sources(&[&key], &[GITLAB], None);
    assert_eq!(check(&net, "1.4.0", None, true, &s).unwrap_err(), TOO_OLD);
    assert!(check(&net, "1.5.0", None, true, &s).unwrap().is_some());
}

#[test]
fn a_redirect_off_the_pinned_hosts_is_refused() {
    let key = release_key();
    let net = FakeNet::default().reply(GITLAB, 302, Some("https://downloads.example.net/recall-update.json"), b"");
    let got = check(&net, "1.9.0", None, true, &sources(&[&key], &[GITLAB], None)).unwrap_err();
    assert!(got.contains("not an official Recall download address"), "{got}");
    assert!(net.calls().iter().all(|c| !c.contains("example.net")), "never requested: {:?}", net.calls());
}

#[test]
fn an_oversized_manifest_is_refused() {
    let key = release_key();
    let mut m = manifest(seq(10), "1.10.0", REV_A);
    m["notes"] = json!("x".repeat(super::manifest::MANIFEST_LIMIT as usize));
    let net = publish(FakeNet::default(), GITLAB, &m, &key);
    assert!(check(&net, "1.9.0", None, true, &sources(&[&key], &[GITLAB], None)).is_err());
}

#[test]
fn the_trust_file_starts_fresh_when_missing_or_garbled_and_keeps_unknown_keys() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("trust.json");
    assert_eq!(TrustState::load(Some(&file)), TrustState::default(), "missing");
    for bytes in [&b""[..], b"{not json", b"[1,2]", br#"{"highest_sequence":"x","revoked":7}"#] {
        std::fs::write(&file, bytes).unwrap();
        let s = TrustState::load(Some(&file));
        assert_eq!((s.highest_sequence, s.revoked.len()), (0, 0), "{}", String::from_utf8_lossy(bytes));
    }
    std::fs::write(&file, br#"{"highest_sequence":5,"revoked":["old-1"],"from_a_newer_build":[1]}"#).unwrap();
    let mut s = TrustState::load(Some(&file));
    s.highest_sequence = 6;
    s.save(&file).unwrap();
    let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved, json!({"highest_sequence": 6, "revoked": ["old-1"], "from_a_newer_build": [1]}));
}

#[test]
fn key_and_tier_files_parse_strictly_and_the_shipped_ones_parse() {
    let key = release_key();
    let line = format!("release release-1 {}", key.pair.pk.to_base64());
    assert_eq!(parse_keys(&format!("# comment\n{line}\n")).unwrap().len(), 1);
    assert!(parse_keys(&format!("{line}\n{line}")).is_err(), "a name twice");
    assert!(parse_keys("release release-1").is_err(), "no key");
    assert!(parse_keys("owner x RWQ=").is_err(), "unknown role");
    assert!(parse_keys("release x notbase64!").is_err());
    assert!(parse_keys(include_str!("update_keys.txt")).is_ok(), "the shipped key file parses");
    let t = parse_tiers(&format!("manifest {GITLAB}\nhost storage.example.org\nmanifest {GITHUB}\n")).unwrap();
    assert_eq!(t.manifests, [GITLAB, GITHUB]);
    assert_eq!(t.hosts, ["gitlab.com", "storage.example.org", "github.com"]);
    for bad in ["manifest http://gitlab.com/x", "manifest https://<user>.example/x", "host a/b", "mirror https://x.org/a"] {
        assert!(parse_tiers(bad).is_err(), "{bad}");
    }
    let shipped = parse_tiers(include_str!("update_tiers.txt")).unwrap();
    assert!(!shipped.manifests.is_empty() && shipped.manifests.iter().all(|m| net::pinned(m).is_ok()), "{shipped:?}");
}

#[test]
fn setup_comes_from_the_first_address_whose_bytes_match() {
    let root = tempfile::tempdir().unwrap();
    let wrong = "https://updates.example.org/v1.10.0/RecallSetup.exe";
    let good = "https://github.com/LoneBeast27/recall/releases/download/v1.10.0/RecallSetup.exe";
    let net = FakeNet::default().reply(wrong, 200, None, b"abd").reply(good, 200, None, b"abc");
    let mut release = super::tests::fixture_release();
    release.urls = vec![SETUP_URL.into(), wrong.into(), good.into()];
    assert!(download_any(&net, &release, root.path()).is_ok());
    assert_eq!(net.calls(), [SETUP_URL, wrong, good]);
    release.urls.pop();
    assert!(download_any(&net, &release, root.path()).is_err(), "no address gave the signed bytes");
}

#[test]
fn check_now_with_no_verified_manifest_says_couldnt_check() {
    let mut ui = UpdateUi::with(Arc::new(FakeNet::default()), Some("1.9.0"), false, true, None);
    let key = release_key();
    ui.sources = Arc::new(sources(&[&key], &tiers3(), None));
    ui.start_check(true);
    let end = Instant::now() + Duration::from_secs(5);
    while ui.busy() && Instant::now() < end {
        ui.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(ui.notice, Some((false, words::COULDNT_CHECK.into())));
}

#[test]
fn a_far_future_sequence_is_refused_and_cannot_lock_out_the_backup_key() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("trust.json");
    let release = release_key();
    let backup = Signer::new(Role::Backup, "backup-1");
    let pinned = [release.pinned()];
    for bad in [u64::MAX, 999_912_312_359, 202613010000, 202611320000, 202611212400, 202611210060, 1] {
        let b = manifest(bad, "1.10.0", REV_A).to_string().into_bytes();
        assert!(verify(&b, &release.sign(&b), &pinned, &[]).is_err(), "sequence {bad}");
    }
    // A leaked release key signs the latest valid sequence; the app accepts it once.
    let s = sources(&[&release, &backup], &[GITLAB], Some(&file));
    let leaked = publish(FakeNet::default(), GITLAB, &manifest(210012312359, "1.10.0", REV_A), &release);
    check(&leaked, "1.9.0", None, true, &s).unwrap();
    // The backup key's revocation still gets in, and resets the sequence to its own.
    let mut m = manifest(seq(30), "1.10.1", REV_A);
    m["revoke_keys"] = json!(["release-1"]);
    check(&publish(FakeNet::default(), GITLAB, &m, &backup), "1.9.0", None, true, &s).unwrap();
    let state = TrustState::load(Some(&file));
    assert_eq!((state.highest_sequence, state.revoked.as_slice()), (seq(30), ["release-1".to_owned()].as_slice()));
    // The replacement release key (a new build pins it) is admitted from there.
    let replacement = Signer::new(Role::Release, "release-2");
    let s2 = sources(&[&replacement, &backup], &[GITLAB], Some(&file));
    let next = publish(FakeNet::default(), GITLAB, &manifest(seq(40), "1.10.2", REV_A), &replacement);
    assert_eq!(check(&next, "1.10.1", None, true, &s2).unwrap().unwrap().version, "1.10.2");
}

#[test]
fn reinstall_takes_this_build_or_a_hotfix_that_names_it() {
    let key = release_key();
    let s = sources(&[&key], &[GITLAB], None);
    let mut m = manifest(seq(10), "1.10.0", REV_B);
    m["supersedes"] = json!([REV_A]);
    let net = publish(FakeNet::default(), GITLAB, &m, &key);
    assert_eq!(this_version(&net, "1.10.0", Some(REV_B), &s).unwrap().hotfix, None, "this very build");
    assert_eq!(this_version(&net, "1.10.0", Some(REV_A), &s).unwrap().hotfix.as_deref(), Some(REV_B), "the hotfix that replaces it");
    let other = "3333333333333333333333333333333333333333";
    assert_eq!(this_version(&net, "1.10.0", Some(other), &s).unwrap_err(), NOT_THIS_VERSION, "never another build");
    assert_eq!(this_version(&net, "1.9.0", Some(REV_A), &s).unwrap_err(), NOT_THIS_VERSION);
    assert_eq!(this_version(&net, "1.10.0", None, &s).unwrap().hotfix, None, "an unstamped local build");
}

//! The keys this build ships (`update_keys.txt`): both roles present, parsed, and the minisign
//! key ids the maintainer published. Replacing a key means changing the file and this test.
use super::manifest::{Role, parse_keys, pinned_keys, verify};

const KEYS: &str = include_str!("update_keys.txt");

/// A minisign public key is "Ed", then the 8-byte key id (little-endian), then 32 key bytes.
fn key_id(base64: &str) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut bits, mut acc, mut out) = (0u32, 0u32, Vec::new());
    for c in base64.bytes().take_while(|&c| c != b'=') {
        acc = (acc << 6) | ABC.iter().position(|&a| a == c).expect("base64") as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    assert_eq!((out.len(), &out[..2]), (42, &b"Ed"[..]), "a minisign Ed25519 public key");
    out[2..10].iter().rev().map(|b| format!("{b:02X}")).collect()
}

#[test]
fn this_build_pins_the_published_release_and_backup_keys() {
    let parsed = parse_keys(KEYS).expect("update_keys.txt parses");
    assert_eq!(parsed.len(), pinned_keys().len(), "the build loads every key in the file");
    let found: Vec<(Role, &str)> = parsed.iter().map(|k| (k.role, k.name.as_str())).collect();
    assert_eq!(found, [(Role::Release, "release-1"), (Role::Backup, "backup-1")]);
    let ids: Vec<String> = KEYS
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .map(|l| key_id(l.split_whitespace().nth(2).unwrap()))
        .collect();
    assert_eq!(ids, ["90822EEE4977EFE7", "F9FD22493A80A43E"]);
}

/// A signed manifest about to be published (scripts/release/sign-and-publish.ps1 runs this with
/// `--ignored`): its signature must verify with the release key this build pins, and every field
/// rule must pass. RECALL_CHECK_MANIFEST names recall-update.json (its .minisig beside it);
/// RECALL_CHECK_SHA256 is the Setup digest it must name.
#[test]
#[ignore = "release check: needs RECALL_CHECK_MANIFEST"]
fn a_signed_manifest_verifies_with_the_pinned_release_key() {
    let path = std::env::var("RECALL_CHECK_MANIFEST").expect("RECALL_CHECK_MANIFEST");
    let json = std::fs::read(&path).unwrap();
    let signature = std::fs::read(format!("{path}.minisig")).unwrap();
    let m = verify(&json, &signature, &pinned_keys(), &[]).expect("the pinned keys and the app's field rules accept it");
    assert_eq!((m.signer, m.signer_name.as_str()), (Role::Release, "release-1"), "signed by the release key");
    if let Ok(sha) = std::env::var("RECALL_CHECK_SHA256") {
        assert_eq!(m.sha256, sha.to_ascii_lowercase(), "the Setup it names");
    }
    println!("APP-VERIFIED version={} sequence={} sha256={} urls={}", m.version, m.sequence, m.sha256, m.urls.len());
}

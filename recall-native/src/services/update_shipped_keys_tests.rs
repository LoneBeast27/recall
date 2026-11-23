//! The keys this build ships (`update_keys.txt`): both roles present, parsed, and the minisign
//! key ids the maintainer published. Replacing a key means changing the file and this test.
use super::manifest::{Role, parse_keys, pinned_keys};

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

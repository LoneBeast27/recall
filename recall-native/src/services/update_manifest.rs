//! The signed update manifest (`recall-update.json` + `.minisig`): the only thing the updater
//! trusts. Its minisign signature is checked against the pinned public keys
//! (`update_keys.txt`) before a single field is read; a host that serves it adds no trust.
//! Rules after the signature: schema 1, the stable channel, a Setup asset with a valid size
//! and SHA-256, at least one pinned download address, never an older `sequence` than this PC
//! has already seen (`update_trust.rs`).
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;

use super::{MAX_SETUP, SETUP, net, version};

/// The largest manifest or signature read (a manifest is a few KiB of JSON and notes).
pub(crate) const MANIFEST_LIMIT: u64 = 256 * 1024;
pub(crate) const SIGNATURE_LIMIT: u64 = 4096;
/// At most this many download addresses are kept from one manifest.
const MAX_URLS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Signs every update.
    Release,
    /// Signs an update only to replace a lost or leaked release key; only it can revoke keys.
    Backup,
}

#[derive(Debug, Clone)]
pub(crate) struct PinnedKey {
    pub role: Role,
    pub name: String,
    key: PublicKey,
}

impl PinnedKey {
    pub(crate) fn new(role: Role, name: &str, base64: &str) -> Result<Self, String> {
        let key = PublicKey::from_base64(base64).map_err(|e| format!("Pinned key {name} is invalid: {e}"))?;
        Ok(Self { role, name: name.into(), key })
    }
}

/// The keys in `text` (the `update_keys.txt` layout). A malformed line is an error, so a typo
/// never silently drops a key.
pub(crate) fn parse_keys(text: &str) -> Result<Vec<PinnedKey>, String> {
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [role, name, base64] = parts[..] else { return Err(format!("Bad key line: {line}")) };
        let role = match role {
            "release" => Role::Release,
            "backup" => Role::Backup,
            other => return Err(format!("Unknown key role: {other}")),
        };
        if keys.iter().any(|k: &PinnedKey| k.name == name) {
            return Err(format!("Key name {name} is listed twice."));
        }
        keys.push(PinnedKey::new(role, name, base64)?);
    }
    Ok(keys)
}

/// This build's pinned keys. Empty until the maintainer adds them: every check then fails.
pub(crate) fn pinned_keys() -> Vec<PinnedKey> {
    parse_keys(include_str!("update_keys.txt")).unwrap_or_default()
}

/// Unknown fields are ignored (not refused), so a later build's manifest still reaches this one.
#[derive(Debug, Clone, Deserialize, PartialEq)]
struct SetupJson {
    name: String,
    size: u64,
    sha256: String,
    urls: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ManifestJson {
    schema: u32,
    channel: String,
    sequence: u64,
    version: String,
    revision: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    min_version: Option<String>,
    setup: SetupJson,
    #[serde(default)]
    revoke_keys: Vec<String>,
    /// Same-version builds this one replaces (a hotfix): a build is offered a same-version
    /// update only when its own revision is listed, so a lagging tier never offers an older build.
    #[serde(default)]
    supersedes: Vec<String>,
}

/// A manifest whose signature and fields passed every rule.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Manifest {
    pub sequence: u64,
    pub version: String,
    pub revision: String,
    pub title: String,
    pub notes: String,
    pub min_version: Option<String>,
    pub size: u64,
    pub sha256: String,
    pub urls: Vec<String>,
    /// Key names to revoke: honoured only when the backup key signed (`signer`).
    pub revoke_keys: Vec<String>,
    pub supersedes: Vec<String>,
    pub signer: Role,
    pub signer_name: String,
}

/// Why a manifest was refused (logged; the screen says "couldn't check").
pub(crate) const BAD_SIGNATURE: &str = "The update file's signature did not verify.";

/// Verify `signature` over `json` with a pinned, unrevoked key, then read and check the fields.
pub(crate) fn verify(json: &[u8], signature: &[u8], keys: &[PinnedKey], revoked: &[String]) -> Result<Manifest, String> {
    let text = std::str::from_utf8(signature).map_err(|_| BAD_SIGNATURE.to_owned())?;
    let signature = Signature::decode(text).map_err(|_| BAD_SIGNATURE.to_owned())?;
    let signer = keys
        .iter()
        .filter(|k| !revoked.contains(&k.name))
        .find(|k| k.key.verify(json, &signature, false).is_ok())
        .ok_or(BAD_SIGNATURE)?;
    let m: ManifestJson =
        serde_json::from_slice(json).map_err(|e| format!("The update file could not be read: {e}"))?;
    if m.schema != 1 {
        return Err(format!("The update file uses an unknown layout ({}).", m.schema));
    }
    if m.channel != "stable" {
        return Err(format!("The update file is for another channel ({}).", m.channel));
    }
    if version(&m.version).is_none() || m.min_version.as_deref().is_some_and(|v| version(v).is_none()) {
        return Err("The update file names an invalid version.".into());
    }
    if !super::hotfix::is_commit(&m.revision) {
        return Err("The update file names an invalid source revision.".into());
    }
    let s = &m.setup;
    if s.name != SETUP {
        return Err("The update file has no Windows Setup.".into());
    }
    if s.size == 0 || s.size > MAX_SETUP {
        return Err("The reported Setup size is invalid.".into());
    }
    if s.sha256.len() != 64 || !s.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The update file has no valid Setup checksum.".into());
    }
    let urls: Vec<String> = s.urls.iter().filter(|u| net::pinned(u).is_ok()).take(MAX_URLS).cloned().collect();
    if urls.is_empty() {
        return Err("The update file names no official download address.".into());
    }
    Ok(Manifest {
        sequence: m.sequence,
        title: m.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("Recall {}", m.version)),
        version: m.version,
        revision: m.revision,
        notes: m.notes.unwrap_or_default(),
        min_version: m.min_version,
        size: s.size,
        sha256: s.sha256.to_ascii_lowercase(),
        urls,
        revoke_keys: m.revoke_keys,
        supersedes: m.supersedes.into_iter().map(|r| r.to_ascii_lowercase()).collect(),
        signer: signer.role,
        signer_name: signer.name.clone(),
    })
}

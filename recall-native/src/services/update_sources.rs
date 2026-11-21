//! Where the signed manifest comes from (`update_tiers.txt`), tier by tier. An automatic check
//! stops at the first tier whose manifest verifies; "Check now" asks every tier and keeps the
//! highest `sequence`, so a tier that lags or hides a release loses. Each tier gets one short
//! try (`TIER_TIMEOUT`): the next tier is the retry. Trust comes only from the signature.
use std::{
    path::PathBuf,
    sync::OnceLock,
    time::Duration,
};

use super::{
    manifest::{self, MANIFEST_LIMIT, Manifest, PinnedKey, SIGNATURE_LIMIT, Role},
    net,
    trust::{self, TrustState},
};

/// One tier's whole limit for the manifest or its signature (a dead first address included).
pub(crate) const TIER_TIMEOUT: Duration = Duration::from_secs(20);
/// The start of the reason when no tier gave a verified manifest (the screen: "couldn't check").
pub(crate) const NO_SOURCE: &str = "No update source gave a verified update file.";

/// What a check trusts and asks: the pinned keys, the tiers in order, and the trust file.
#[derive(Debug, Clone)]
pub(crate) struct Sources {
    pub keys: Vec<PinnedKey>,
    pub tiers: Vec<String>,
    pub trust_file: Option<PathBuf>,
}

impl Sources {
    /// This build's keys and tiers, and the per-user trust file.
    pub(crate) fn pinned() -> Self {
        Self { keys: manifest::pinned_keys(), tiers: tiers().manifests.clone(), trust_file: trust::trust_file() }
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Tiers {
    pub manifests: Vec<String>,
    /// Every host the tiers use: their own and the hosts their downloads redirect to.
    pub hosts: Vec<String>,
}

/// The tiers in `text` (the `update_tiers.txt` layout).
pub(crate) fn parse_tiers(text: &str) -> Result<Tiers, String> {
    let mut out = Tiers::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (kind, value) = line.split_once(char::is_whitespace).ok_or_else(|| format!("Bad tier line: {line}"))?;
        let value = value.trim();
        let host = match kind {
            "manifest" => {
                let host = value
                    .strip_prefix("https://")
                    .and_then(|r| r.split('/').next())
                    .filter(|h| !h.is_empty() && !h.contains(['@', ':', '<', '>']))
                    .ok_or_else(|| format!("Bad tier address: {value}"))?;
                out.manifests.push(value.to_owned());
                host.to_ascii_lowercase()
            }
            "host" if !value.is_empty() && !value.contains(['/', '@', ':', '<', '>']) => value.to_ascii_lowercase(),
            _ => return Err(format!("Bad tier line: {line}")),
        };
        if !out.hosts.contains(&host) {
            out.hosts.push(host);
        }
    }
    Ok(out)
}

/// This build's tiers (empty if the file is malformed: every check then fails, closed).
pub(crate) fn tiers() -> &'static Tiers {
    static TIERS: OnceLock<Tiers> = OnceLock::new();
    TIERS.get_or_init(|| parse_tiers(include_str!("update_tiers.txt")).unwrap_or_default())
}

/// One small GET with one try and the tier's limit; anything but 200 is a failure.
fn get(net: &dyn net::Transport, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let status = net::fetch(net, url, limit, TIER_TIMEOUT, &mut body);
    net::with_progress(|p| p.answered());
    match status? {
        200 => Ok(body),
        status => Err(format!("HTTP {status}")),
    }
}

/// The manifest at `url` and its signature, verified.
fn fetch_one(net: &dyn net::Transport, url: &str, keys: &[PinnedKey], revoked: &[String]) -> Result<Manifest, String> {
    let json = get(net, url, MANIFEST_LIMIT)?;
    let signature = get(net, &format!("{url}.minisig"), SIGNATURE_LIMIT)?;
    manifest::verify(&json, &signature, keys, revoked)
}

/// The newest verified manifest the tiers give (all of them when `all`), after revocations
/// and the replay rule. Accepting it updates the trust file.
pub(crate) fn find(net: &dyn net::Transport, sources: &Sources, all: bool) -> Result<Manifest, String> {
    if all {
        // "Check now": every tier's manifest and signature, for the bar.
        net::with_progress(|p| p.plan(u32::try_from(sources.tiers.len() * 2).unwrap_or(u32::MAX)));
    }
    let file = sources.trust_file.as_deref();
    let mut state = TrustState::load(file);
    let mut found: Vec<Manifest> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    for url in &sources.tiers {
        match fetch_one(net, url, &sources.keys, &state.revoked) {
            Ok(m) if state.admits(&m) => {
                found.push(m);
                if !all {
                    break;
                }
            }
            Ok(m) => reasons.push(format!("{url}: older than one already accepted ({} < {})", m.sequence, state.highest_sequence)),
            Err(e) => reasons.push(format!("{url}: {e}")),
        }
    }
    // Revocations first (backup-signed only), then drop what a revoked key signed.
    let mut changed = false;
    for m in found.iter().filter(|m| m.signer == Role::Backup) {
        changed |= state.accept(m);
    }
    found.retain(|m| !state.revoked.contains(&m.signer_name));
    let best = found.into_iter().max_by_key(|m| m.sequence);
    if let Some(m) = &best {
        changed |= state.accept(m);
    }
    if changed {
        if let Some(file) = file {
            if let Err(e) = state.save(file) {
                eprintln!("update: could not save {}: {e}", file.display());
            }
        }
    }
    best.ok_or_else(|| {
        if sources.keys.is_empty() {
            reasons.insert(0, "this build has no pinned update keys".into());
        }
        format!("{NO_SOURCE} {}", reasons.join("; "))
    })
}

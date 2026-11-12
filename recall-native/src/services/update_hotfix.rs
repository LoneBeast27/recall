//! Same-version hotfixes: a release whose version equals this build's but whose source commit
//! differs. Release builds stamp their commit (`RECALL_SOURCE_REVISION`); the release carries
//! `SOURCE-REVISION.txt` and `SHA256SUMS.txt`. A hotfix is offered only when its revision is
//! the tag's commit, is newer than this build's (never a same-number downgrade), and its
//! `SHA256SUMS.txt` lists the Setup checksum GitHub reports. Same revision: never offered.
use std::time::Duration;

use super::{REPO, Release, SETUP, api, net};

const SMALL: u64 = 4096;

/// This build's source commit, or `None` (a dev or local build: no same-version offers).
pub fn revision() -> Option<&'static str> {
    stamped(option_env!("RECALL_SOURCE_REVISION"))
}

pub(crate) fn stamped(text: Option<&'static str>) -> Option<&'static str> {
    text.filter(|r| is_commit(r))
}

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn asset(net: &dyn net::Transport, version: &str, name: &str) -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases/download/v{version}/{name}");
    let mut body = Vec::new();
    match net::fetch(net, &url, SMALL, Duration::from_secs(20), &mut body)? {
        200 => String::from_utf8(body).map_err(|_| format!("{name} is not text.")),
        status => Err(format!("{name}: HTTP {status}.")),
    }
}

/// The commit `SOURCE-REVISION.txt` names (first line, 40 lowercase hex).
pub(crate) fn parse_revision(text: &str) -> Option<String> {
    let line = text.trim_start_matches('\u{feff}').lines().next()?.trim().to_ascii_lowercase();
    is_commit(&line).then_some(line)
}

/// `SHA256SUMS.txt` lists `digest` for RecallSetup.exe.
pub(crate) fn sums_match(text: &str, digest: &str) -> bool {
    text.trim_start_matches('\u{feff}').lines().any(|line| {
        let mut parts = line.split_whitespace();
        let sum = parts.next().unwrap_or_default();
        let name = parts.next().map(|n| n.trim_start_matches('*'));
        sum.eq_ignore_ascii_case(digest) && name == Some(SETUP)
    })
}

/// `SHA256SUMS.txt` of the release lists the Setup checksum GitHub reported.
pub(crate) fn verify_sums(net: &dyn net::Transport, release: &Release) -> Result<(), String> {
    if sums_match(&asset(net, &release.version, "SHA256SUMS.txt")?, &release.digest) {
        Ok(())
    } else {
        Err("Setup does not match the release's SHA256SUMS.txt.".into())
    }
}

#[derive(serde::Deserialize)]
struct Comparison {
    status: String,
    behind_by: u64,
}

fn compare(net: &dyn net::Transport, base: &str, head: &str) -> Result<Option<String>, String> {
    let (status, body) = api(net, &format!("compare/{base}...{head}?per_page=1&page=2"))?;
    if status != 200 {
        return Ok(None);
    }
    let c: Comparison = serde_json::from_slice(&body).map_err(|e| format!("Invalid GitHub comparison: {e}"))?;
    Ok((c.behind_by == 0).then_some(c.status))
}

/// The hotfix's revision when `release` (same version as this build, tag already on main) is
/// a different, newer build than `ours`; `None` when it is this build or not newer.
pub(crate) fn newer_build(net: &dyn net::Transport, release: &Release, ours: &str) -> Result<Option<String>, String> {
    let Some(theirs) = parse_revision(&asset(net, &release.version, "SOURCE-REVISION.txt")?) else {
        return Err("The release's SOURCE-REVISION.txt is invalid.".into());
    };
    if theirs == ours {
        return Ok(None);
    }
    // The revision is the tag's own commit, so it is the build that tag on main names.
    if compare(net, &format!("v{}", release.version), &theirs)?.as_deref() != Some("identical") {
        return Err("The hotfix's source revision is not its release tag.".into());
    }
    // Strictly after this build's commit: an older or unrelated build is never offered.
    if compare(net, ours, &theirs)?.as_deref() != Some("ahead") {
        return Ok(None);
    }
    Ok(Some(theirs))
}

/// What "Skip" remembers: the version, or the version and build for a hotfix.
pub(crate) fn skip_key(release: &Release) -> String {
    match &release.hotfix {
        Some(rev) => format!("{}+{rev}", release.version),
        None => release.version.clone(),
    }
}

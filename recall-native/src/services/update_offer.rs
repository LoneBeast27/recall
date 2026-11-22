//! What a verified manifest offers this build (`check`): a newer version, a same-version hotfix
//! that names this build, or nothing; and the Setup download from its addresses in turn.
use super::{Release, hotfix, manifest::Manifest, net, setup, skipped, sources, version};

/// Why a check stops when this build is too old to take the update in one step.
pub(crate) const TOO_OLD: &str = "This update needs a fresh install of Recall.";

/// The startup check and "Check now" alike: the newest verified manifest across every tier,
/// then the offer rules (`manual` only decides whether a skipped version is shown). `revision` is this build's source commit
/// (`hotfix::revision`): without it a same-version release is never offered.
pub(super) fn check(net: &dyn net::Transport, current: &str, revision: Option<&str>, manual: bool, sources: &sources::Sources) -> Result<Option<Release>, String> {
    let m = sources::find(net, sources)?;
    let (Some(theirs), Some(ours)) = (version(&m.version), version(current)) else { return Ok(None) };
    let same = theirs == ours;
    if theirs < ours {
        return Ok(None);
    }
    // A hotfix only replaces a build it names: never an older build of the same number.
    if same && !revision.is_some_and(|r| r != m.revision && m.supersedes.iter().any(|s| s == r)) {
        return Ok(None);
    }
    if m.min_version.as_deref().and_then(version).is_some_and(|min| ours < min) {
        return Err(TOO_OLD.into());
    }
    let revision = m.revision.clone();
    let mut release = release_from(m);
    if same {
        release.title = format!("A fix for Recall {current}");
        release.hotfix = Some(revision);
    }
    if !manual && skipped().as_deref() == Some(hotfix::skip_key(&release).as_str()) {
        return Ok(None);
    }
    Ok(Some(release))
}

/// Setup from the release's addresses in turn: a failed or mismatched download moves on to the
/// next one. The last failure is returned when none gave the signed size and SHA-256.
pub(super) fn download_any(net: &dyn net::Transport, release: &Release, root: &std::path::Path) -> Result<setup::Verified, String> {
    let mut last = Err("The update names no download address.".to_owned());
    for url in &release.urls {
        last = setup::download(net, url, release, root);
        match &last {
            Ok(_) => break,
            Err(e) => eprintln!("update: download from {url} failed: {e}"),
        }
    }
    last
}

/// The release a verified manifest describes: Setup's size, SHA-256 and addresses.
fn release_from(m: Manifest) -> Release {
    Release { version: m.version, title: m.title, notes: m.notes, size: m.size, digest: m.sha256, hotfix: None, urls: m.urls }
}

/// Why Reinstall stops: the signed update names another version than this one.
pub(crate) const NOT_THIS_VERSION: &str = "The signed update is for another version of Recall.";

/// Reinstall: this build's Setup, from the newest verified manifest only (never from an
/// unsigned source). The manifest must be this version and either this build (`revision`) or
/// a hotfix that names it; a local build without a stamp takes the signed build of its version.
pub(crate) fn this_version(net: &dyn net::Transport, current: &str, revision: Option<&str>, sources: &sources::Sources) -> Result<Release, String> {
    let m = sources::find(net, sources)?;
    if m.version != current {
        return Err(NOT_THIS_VERSION.into());
    }
    let hotfix = match revision {
        Some(ours) if ours == m.revision => None,
        Some(ours) if m.supersedes.iter().any(|s| s == ours) => Some(m.revision.clone()),
        Some(_) => return Err(NOT_THIS_VERSION.into()),
        None => None,
    };
    let mut release = release_from(m);
    // A hotfix build: the reopened app checks it became that build (`after_update`).
    release.hotfix = hotfix;
    Ok(release)
}

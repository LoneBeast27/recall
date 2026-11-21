//! What a verified manifest offers this build (`check`): a newer version, a same-version hotfix
//! that names this build, or nothing; and the Setup download from its addresses in turn.
use super::{Release, hotfix, net, setup, skipped, sources, version};

/// Why a check stops when this build is too old to take the update in one step.
pub(crate) const TOO_OLD: &str = "This update needs a fresh install of Recall.";

/// The startup check and "Check now" alike: the newest verified manifest (every tier when
/// `manual`), then the offer rules. `revision` is this build's source commit
/// (`hotfix::revision`): without it a same-version release is never offered.
pub(super) fn check(net: &dyn net::Transport, current: &str, revision: Option<&str>, manual: bool, sources: &sources::Sources) -> Result<Option<Release>, String> {
    let m = sources::find(net, sources, manual)?;
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
    let mut release = Release {
        version: m.version.clone(),
        title: m.title,
        notes: m.notes,
        size: m.size,
        digest: m.sha256,
        hotfix: None,
        urls: m.urls,
    };
    if same {
        release.title = format!("A fix for Recall {current}");
        release.hotfix = Some(m.revision);
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


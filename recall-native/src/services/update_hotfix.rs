//! Same-version hotfixes: a signed manifest whose version equals this build's, whose source
//! commit differs, and which lists this build's commit in `supersedes` (so an older build of
//! the same number is never offered). Release builds stamp their commit
//! (`RECALL_SOURCE_REVISION`). Builds without a stamp never get a same-version offer.

use super::Release;

/// This build's source commit, or `None` (a dev or local build: no same-version offers).
pub fn revision() -> Option<&'static str> {
    stamped(option_env!("RECALL_SOURCE_REVISION"))
}

pub(crate) fn stamped(text: Option<&'static str>) -> Option<&'static str> {
    text.filter(|r| is_commit(r))
}

pub(crate) fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn refused_file() -> Option<std::path::PathBuf> {
    crate::user_dirs::local_appdata().map(|p| crate::user_dirs::checked(p.join("Recall").join("updates").join("refused.txt")))
}

/// The hotfix (`version+commit`) an update did not install as expected, if any.
pub(crate) fn load_refused() -> Option<String> {
    let text = std::fs::read_to_string(refused_file()?).ok()?;
    Some(text.trim().to_owned()).filter(|k| !k.is_empty())
}

pub(crate) fn save_refused(key: &str) {
    if let Some(path) = refused_file() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent).and_then(|_| std::fs::write(&path, key));
        }
    }
}

/// What "Skip" remembers: the version, or the version and build for a hotfix.
pub(crate) fn skip_key(release: &Release) -> String {
    match &release.hotfix {
        Some(rev) => format!("{}+{rev}", release.version),
        None => release.version.clone(),
    }
}

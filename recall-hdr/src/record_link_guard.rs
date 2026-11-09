//! Recall never records into a linked recording folder (SteelSeries Moments, ShadowPlay...):
//! those folders are read-only to Recall. The app's folder list (`%APPDATA%\Recall\folders.json`,
//! `RECALL_FOLDERS` overrides the path; written by recall-native `services::library::folders`)
//! names them: `imported` minus `former_recordings`. Read at start and on every config change.
//!
//! `recording.output_dir` is refused when it
//! - is a linked folder;
//! - holds a linked folder (recordings above a link);
//! - lies inside a linked folder and is not a folder Recall already records into (`recordings`
//!   or a `former_recordings` entry): a parent of the recordings folder may be linked
//!   (Videos linked, clips in Videos\Recall), a move into someone else's part of it may not.
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::log;

/// What folders.json says about linked and owned folders.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Links {
    pub linked: Vec<String>,
    /// The recordings folder and the former ones: folders Recall already records into.
    pub owned: Vec<String>,
}

fn folders_path() -> Option<PathBuf> {
    // Unit tests never read the user's real folder list.
    if cfg!(test) {
        return None;
    }
    if let Some(path) = std::env::var_os("RECALL_FOLDERS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(|root| PathBuf::from(root).join("Recall").join("folders.json"))
}

/// The app's folder list now. Missing or unreadable: no links (logged when unreadable).
pub(crate) fn read_links() -> Links {
    folders_path().map(|p| read_links_from(&p)).unwrap_or_default()
}

pub(crate) fn read_links_from(path: &Path) -> Links {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Links::default(),
        Err(e) => {
            log!("record: folder list {} unreadable ({e}); linked folders unknown", path.display());
            return Links::default();
        }
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        log!("record: folder list {} is not valid JSON; linked folders unknown", path.display());
        return Links::default();
    };
    let list = |key: &str| -> Vec<String> {
        v.get(key).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str()).map(str::to_owned).collect()).unwrap_or_default()
    };
    let former = list("former_recordings");
    let linked = list("imported").into_iter().filter(|f| !former.iter().any(|o| key(o) == key(f))).collect();
    let owned = v.get("recordings").and_then(|r| r.as_str()).map(str::to_owned).into_iter().chain(former).collect();
    Links { linked, owned }
}

/// Comparable form: the real path of the deepest existing ancestor (junctions resolved) plus the
/// rest; `\`-separated, no trailing `\`, lowercase.
fn key(path: &str) -> String {
    let p = Path::new(path.trim());
    let absolute = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let real = absolute
        .ancestors()
        .find_map(|a| {
            let real = std::fs::canonicalize(a).ok()?;
            let text = real.to_string_lossy();
            let plain = text.strip_prefix(r"\\?\UNC\").map(|s| format!(r"\\{s}")).or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))?;
            Some(PathBuf::from(plain).join(absolute.strip_prefix(a).ok()?))
        })
        .unwrap_or(absolute);
    real.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn within(inner: &str, outer: &str) -> bool {
    inner == outer || (inner.starts_with(outer) && inner.as_bytes().get(outer.len()) == Some(&b'\\'))
}

/// Why Recall may not record into `dir`, if it may not.
pub(crate) fn refusal(dir: &str, links: &Links) -> Option<String> {
    let d = key(dir);
    let owned = links.owned.iter().any(|o| key(o) == d);
    links.linked.iter().find_map(|link| {
        let l = key(link);
        let refused = d == l || within(&l, &d) || (within(&d, &l) && !owned);
        refused.then(|| format!("{dir} is a linked folder; Recall won't record into it"))
    })
}

/// Start, and every config change: an output folder [`refusal`] refuses becomes the last good
/// one (`last_good`, else the app's recordings folder, else the default), and when every one of
/// those is refused, Recall's own [`own_clips_dir`]. A refused folder is never kept. Returns the
/// notice.
pub(crate) fn guard(cfg: &mut Config, last_good: Option<&str>, links: &Links) -> Option<String> {
    let why = refusal(&cfg.recording.output_dir, links)?;
    let default = crate::record_config::default_output_dir();
    let candidates = last_good.into_iter().map(str::to_owned).chain(links.owned.first().cloned()).chain([default]);
    if let Some(fallback) = candidates.filter(|c| !c.trim().is_empty()).find(|c| refusal(c, links).is_none()) {
        log!("record: {why}; recording into {fallback:?} instead");
        cfg.recording.output_dir = fallback.clone();
        return Some(format!("{why}. Clips save to {fallback}."));
    }
    // Recall's own folder: never refused, even inside a linked tree.
    let own = own_clips_dir().to_string_lossy().into_owned();
    log!("record: {why}; no other folder is free of linked folders, recording into {own:?}");
    cfg.recording.output_dir = own.clone();
    Some(format!("Recordings folder unavailable, saving to {own}"))
}

/// `%LOCALAPPDATA%\Recall\Clips` (the LocalAppData known folder), created when missing. Without a
/// LocalAppData folder (never on a working profile): `<temp>\Recall\Clips`.
pub(crate) fn own_clips_dir() -> PathBuf {
    let base = crate::user_dirs::known(crate::user_dirs::Known::LocalAppData).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Recall").join("Clips");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log!("record: could not create {} ({e})", dir.display());
    }
    dir
}

#[cfg(test)]
#[path = "record_link_guard_tests.rs"]
mod tests;

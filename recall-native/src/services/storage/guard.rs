//! The cache and the clip folders must not overlap. A recordings or library folder inside the
//! cache (or the cache inside one) would put the user's clips where eviction and the sweeps
//! work, so such a setup is refused: Settings will not pick that folder, and at start the
//! app [`block`]s the cache. A blocked cache deletes nothing and takes no new video copies
//! (clips play from where they lie) until the folders are apart and the app restarts.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// (normalized cache folder, reason) of every blocked cache.
static BLOCKED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Comparable form: the real path when it exists (junctions and links resolved, its real
/// case), else the absolute path; `\`-separated, no trailing `\`, lowercase.
fn normalized(path: &Path) -> String {
    let real = std::fs::canonicalize(path).map(|p| super::plain_path(&p)).unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    real.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// `inner` is `outer` or lies inside it (both normalized).
fn within(inner: &str, outer: &str) -> bool {
    inner == outer || (inner.starts_with(outer) && inner.as_bytes().get(outer.len()) == Some(&b'\\'))
}

/// The first clip folder that overlaps a cache folder, as a sentence for the user; `None`
/// when they are all apart. Empty paths are ignored.
pub fn overlap(cache: &[PathBuf], clips: &[PathBuf]) -> Option<String> {
    for cache_dir in cache.iter().filter(|p| !p.as_os_str().is_empty()) {
        let cache_key = normalized(cache_dir);
        for clip_dir in clips.iter().filter(|p| !p.as_os_str().is_empty()) {
            let clip_key = normalized(clip_dir);
            // The text, or the folders themselves (`\\nas\x` and `\\10.0.0.5\x`, junctions).
            let inside = |inner: &Path, outer: &Path| crate::file_id::inside(inner, outer) == Some(true);
            let how = if within(&clip_key, &cache_key) || inside(clip_dir, cache_dir) {
                "is inside Recall's cache folder"
            } else if within(&cache_key, &clip_key) || inside(cache_dir, clip_dir) {
                "holds Recall's cache folder"
            } else {
                continue;
            };
            return Some(format!("The clip folder {} {how} ({}).", clip_dir.display(), cache_dir.display()));
        }
    }
    None
}

/// Every folder the app caches into: the cache root, and the engine's `<exe folder>\cache`
/// (its staged and kept copies) when that differs.
pub fn cache_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![super::cache_root()];
    let engine = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("cache")));
    if let Some(engine) = engine.filter(|engine| normalized(engine) != normalized(&dirs[0])) {
        dirs.push(engine);
    }
    dirs
}

/// Settings: why `folder` cannot be a recordings or library folder, if it cannot.
pub fn refuse_clip_folder(folder: &Path) -> Option<String> {
    overlap(&cache_dirs(), &[folder.to_path_buf()]).map(|why| format!("{why} Pick a folder outside it: the cache deletes and replaces files on its own."))
}

/// Start, and whenever a clip folder changes: blocks every cache folder that overlaps one of
/// `clips`, and releases one that no longer does. Returns the message to show, or `None` when
/// all is apart.
pub fn check(cache: &[PathBuf], clips: &[PathBuf]) -> Option<String> {
    let Some(why) = overlap(cache, clips) else {
        let keys: Vec<String> = cache.iter().map(|dir| normalized(dir)).collect();
        BLOCKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).retain(|(dir, _)| !keys.contains(dir));
        return None;
    };
    let message = format!("{why} Until they are apart, Recall deletes nothing in its cache and makes no new copies there; clips play from where they are.");
    for dir in cache {
        block(dir, message.clone());
    }
    eprintln!("cache guard: {message}");
    Some(message)
}

/// Stops every deletion and new copy under `root` for the rest of this run.
pub fn block(root: &Path, reason: String) {
    let key = normalized(root);
    let mut blocked = BLOCKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !blocked.iter().any(|(dir, _)| *dir == key) {
        blocked.push((key, reason));
    }
}

/// Why `path` (a cache root or anything under it) may not be touched, if it is blocked.
pub fn blocked(path: &Path) -> Option<String> {
    let blocked = BLOCKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if blocked.is_empty() {
        return None;
    }
    let key = normalized(path);
    blocked.iter().find(|(dir, _)| within(&key, dir)).map(|(_, reason)| reason.clone())
}

//! Path comparison for the staging guard (`staging::refused`), and the library folders the
//! app lists (`%APPDATA%\Recall\folders.json`, `RECALL_FOLDERS` overrides; written by
//! `recall-native` `services/library/folders.rs`). The engine only reads that file.
use std::path::{Path, PathBuf};

/// Comparable form: the real path when it exists (links resolved, real case), else the
/// absolute path; `\`-separated, no trailing `\`, no `\\?\`, lowercase.
pub(crate) fn normalized(path: &Path) -> String {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    let text = real.to_string_lossy().replace('/', "\\");
    let plain = match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with("UNC\\") => format!(r"\\{}", &rest[4..]),
        Some(rest) => rest.to_owned(),
        None => text,
    };
    plain.trim_end_matches('\\').to_lowercase()
}

/// `inner` is `outer` or lies inside it (both [`normalized`]).
pub(crate) fn within(inner: &str, outer: &str) -> bool {
    inner == outer || (inner.starts_with(outer) && inner.as_bytes().get(outer.len()) == Some(&b'\\'))
}

/// The recordings and imported folders of the app's library, when its file is there.
pub(crate) fn library_dirs() -> Vec<PathBuf> {
    let file = std::env::var_os("RECALL_FOLDERS")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("folders.json"))));
    file.map(|path| parse(&std::fs::read(path).unwrap_or_default())).unwrap_or_default()
}

/// The app's former recordings folders (`former_recordings` in the same file): none when unread.
pub(crate) fn former_recordings() -> Vec<PathBuf> {
    let file = std::env::var_os("RECALL_FOLDERS")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("folders.json"))));
    let Some(bytes) = file.and_then(|path| std::fs::read(path).ok()) else { return Vec::new() };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Vec::new() };
    value.get("former_recordings").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).map(PathBuf::from).collect()
}

/// `{"recordings": "...", "imported": ["..."]}` → those folders (anything unreadable: none).
pub(crate) fn parse(bytes: &[u8]) -> Vec<PathBuf> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else { return Vec::new() };
    let recordings = value.get("recordings").and_then(|v| v.as_str()).map(PathBuf::from);
    let imported = value.get("imported").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).map(PathBuf::from);
    recordings.into_iter().chain(imported).collect()
}

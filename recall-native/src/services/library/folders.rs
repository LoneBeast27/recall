//! Clip folders: the recorder's output folder plus folders the user imported (old clips from
//! ShadowPlay, Medal, OBS, Game Bar...). Persisted at `%APPDATA%\Recall\folders.json`
//! (`RECALL_FOLDERS` overrides the path). Nothing is moved or copied; imported folders are
//! only listed.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Video files the library lists.
pub const CLIP_EXTENSIONS: [&str; 4] = ["mp4", "m4v", "mov", "mkv"];
/// How deep an imported folder is searched (Medal: `Medal\Clips\<game>\clip.mp4`).
const MAX_DEPTH: usize = 4;
/// Upper bound on files listed per imported folder (a mistaken pick of `C:\` stays cheap).
const MAX_FILES: usize = 50_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Folders {
    /// Last output folder the recorder reported, so the library lists the right folder
    /// before the recorder connects.
    #[serde(default)]
    pub recordings: Option<String>,
    #[serde(default)]
    pub imported: Vec<String>,
}

/// One clip found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundClip {
    pub path: PathBuf,
    pub bytes: u64,
    pub mtime: u64,
    /// Name of the folder holding the clip when it sits in a subfolder of an imported
    /// folder (ShadowPlay / Medal keep one folder per game).
    pub folder: Option<String>,
}

fn folders_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RECALL_FOLDERS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(|root| PathBuf::from(root).join("Recall").join("folders.json"))
}

impl Folders {
    pub fn load() -> Self {
        folders_path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub(crate) fn load_from(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = folders_path().ok_or("APPDATA is not set")?;
        self.save_to(&path)
    }

    /// Atomic: write `folders.json.tmp`, then rename over the file.
    pub(crate) fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, bytes).map_err(|e| format!("write folders: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("save folders: {e}"))
    }

    /// Adds a folder; false when it is already listed (or is the recordings folder).
    pub fn add(&mut self, folder: &Path) -> bool {
        let text = folder.to_string_lossy().into_owned();
        let key = folder_key(&text);
        if self.recordings.as_deref().is_some_and(|r| folder_key(r) == key) || self.imported.iter().any(|f| folder_key(f) == key) {
            return false;
        }
        self.imported.push(text);
        true
    }

    pub fn remove(&mut self, folder: &str) {
        let key = folder_key(folder);
        self.imported.retain(|f| folder_key(f) != key);
    }
}

/// Case-insensitive, separator-normalized, no trailing slash.
pub fn folder_key(folder: &str) -> String {
    folder.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// True when `path` lies inside `folder` (both plain absolute paths).
pub fn is_inside(path: &str, folder: &str) -> bool {
    let (path, folder) = (folder_key(path), folder_key(folder));
    path.len() > folder.len() && path.starts_with(&folder) && path.as_bytes()[folder.len()] == b'\\'
}

pub fn is_clip(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| CLIP_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Lists clips in an imported folder and its subfolders (depth-first, bounded). Hidden and
/// system folders, and Recall's own cache, are skipped.
pub fn scan_imported(root: &Path, skip: &[PathBuf]) -> Vec<FoundClip> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else { continue };
        for item in listing.flatten() {
            let Ok(meta) = item.metadata() else { continue };
            let path = item.path();
            if meta.is_dir() {
                if depth + 1 < MAX_DEPTH && !hidden(&meta) && !skip.iter().any(|s| s == &path) && !skipped_name(&item.file_name().to_string_lossy()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !meta.is_file() || !is_clip(&path) {
                continue;
            }
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            let folder = (depth > 0).then(|| dir.file_name().map(|n| n.to_string_lossy().into_owned())).flatten();
            found.push(FoundClip { path, bytes: meta.len(), mtime, folder });
            if found.len() >= MAX_FILES {
                return found;
            }
        }
    }
    found
}

fn hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

/// Recorder bookkeeping folders that never hold clips worth listing.
fn skipped_name(name: &str) -> bool {
    let name = name.to_lowercase();
    matches!(name.as_str(), "thumbs" | "thumbnails" | "$recycle.bin" | "system volume information" | "windows" | "program files" | "program files (x86)" | "node_modules") || name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recall-folders-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_dedupes_case_and_slashes_and_skips_recordings() {
        let mut folders = Folders { recordings: Some(r"V:\ALL RECORDINGS".into()), imported: vec![] };
        assert!(!folders.add(Path::new(r"v:\all recordings\")));
        assert!(folders.add(Path::new(r"D:\ShadowPlay")));
        assert!(!folders.add(Path::new("d:/shadowplay/")));
        assert_eq!(folders.imported, vec![r"D:\ShadowPlay".to_owned()]);
        folders.remove(r"d:\SHADOWPLAY");
        assert!(folders.imported.is_empty());
    }

    #[test]
    fn is_inside_needs_a_separator() {
        assert!(is_inside(r"D:\Clips\Rocket League\a.mp4", r"D:\Clips"));
        assert!(is_inside(r"d:\clips\a.mp4", r"D:\Clips\"));
        assert!(!is_inside(r"D:\Clips2\a.mp4", r"D:\Clips"));
        assert!(!is_inside(r"D:\Clips", r"D:\Clips"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp("roundtrip");
        let path = dir.join("folders.json");
        let folders = Folders { recordings: Some(r"V:\ALL RECORDINGS".into()), imported: vec![r"D:\Old".into()] };
        folders.save_to(&path).unwrap();
        assert_eq!(Folders::load_from(&path), folders);
        assert_eq!(Folders::load_from(&dir.join("missing.json")), Folders::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_finds_nested_clips_with_their_folder() {
        let dir = temp("scan");
        std::fs::create_dir_all(dir.join("Rocket League")).unwrap();
        std::fs::create_dir_all(dir.join("thumbs")).unwrap();
        std::fs::write(dir.join("loose.mp4"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("Rocket League").join("goal.MKV"), b"xy").unwrap();
        std::fs::write(dir.join("thumbs").join("loose.mp4"), b"x").unwrap();
        let found = scan_imported(&dir, &[]);
        assert_eq!(found.len(), 2);
        let by_name = |name: &str| found.iter().find(|f| f.path.file_name().is_some_and(|n| n == name)).unwrap();
        assert_eq!(by_name("loose.mp4").folder, None);
        assert_eq!(by_name("goal.MKV").folder.as_deref(), Some("Rocket League"));
        assert_eq!(by_name("goal.MKV").bytes, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

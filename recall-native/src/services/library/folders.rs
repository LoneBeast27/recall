//! Clip folders: the recorder's output folder plus folders the user imported (old clips from
//! ShadowPlay, Medal, OBS, Game Bar...). Persisted at `%APPDATA%\Recall\folders.json`
//! (`RECALL_FOLDERS` overrides the path). Nothing is moved or copied; imported folders are
//! only listed.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[path = "folders_salvage.rs"]
mod salvage;

/// Video files the library lists.
pub const CLIP_EXTENSIONS: [&str; 4] = ["mp4", "m4v", "mov", "mkv"];
/// How deep an imported folder is searched (Medal: `Medal\Clips\<game>\clip.mp4`).
const MAX_DEPTH: usize = 4;
/// Upper bound on clips listed per imported folder.
const MAX_FILES: usize = 50_000;
/// Upper bound on directory entries walked per imported folder (a mistaken pick of `C:\`
/// stays cheap).
const MAX_ENTRIES: usize = 200_000;
/// An imported folder is re-walked at most this often; new saves land in the recordings
/// folder, which is listed fresh every time.
const SCAN_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// The recorder's default clip folder, `<Videos>\Recall` (the controller's
/// `recording.output_dir` default): the Videos known folder, else `%USERPROFILE%\Videos`.
pub fn default_recordings_dir() -> PathBuf {
    let videos = crate::user_dirs::known(crate::user_dirs::Known::Videos);
    crate::user_dirs::checked(videos.unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos")).join("Recall"))
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Folders {
    /// Last output folder the recorder reported, so the library lists the right folder
    /// before the recorder connects.
    #[serde(default)]
    pub recordings: Option<String>,
    #[serde(default)]
    pub imported: Vec<String>,
    /// Why saving is refused: the file exists but could not be read (an antivirus scan,
    /// a sync client), so a save would replace the user's folders with this empty list.
    #[serde(skip)]
    pub(crate) write_block: Option<String>,
    /// The notice to show once: folders.json was damaged; what was read back of it.
    #[serde(skip)]
    pub damaged: Option<String>,
    /// Folders that were the recordings folder before: their clips stay Recall's own (Delete
    /// recycles them, edits stay beside them) even though they are listed as imported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_recordings: Vec<String>,
}

/// One clip found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundClip {
    pub path: PathBuf,
    pub bytes: u64,
    pub mtime: u64,
    /// Name of the folder holding the clip: the game for ShadowPlay / Medal (one folder per
    /// game), else the imported folder's own name ("Old Valorant clips").
    pub folder: Option<String>,
}

fn folders_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RECALL_FOLDERS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("folders.json")))
}

impl Folders {
    pub fn load() -> Self {
        folders_path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    /// Missing -> none. Damaged -> the folders still readable in it are kept (written back,
    /// the damaged file copied to a dated `.bad`, `folders_salvage.rs`) and
    /// [`Folders::damaged`] says so. Unreadable -> none, and [`Folders::save_to`] refuses to
    /// replace the file.
    pub(crate) fn load_from(path: &Path) -> Self {
        let now = std::time::SystemTime::now();
        crate::services::durable::sweep_temps(path, now);
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Self::recover(path, &bytes, now)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self { write_block: Some(format!("folders.json could not be read ({error}); not saved")), ..Self::default() },
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = folders_path().ok_or("APPDATA is not set")?;
        self.save_to(&path)
    }

    /// Durable: a temp file, flushed, then renamed over the file.
    pub(crate) fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(reason) = &self.write_block {
            return Err(reason.clone());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        // Keys a newer build wrote stay (`json_keep`).
        let bytes = crate::services::json_keep::pretty_keeping(path, self, &["recordings", "imported", "former_recordings"])?;
        crate::services::durable::write_durable(path, &bytes).map_err(|e| format!("save folders: {e}"))
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

    /// Settings → Clip folders → remove: the folder leaves the library and is no longer one
    /// Recall manages (re-adding it later lists it as an ordinary imported folder). A folder that
    /// overlaps the recordings folder stays managed ([`Folders::release`]).
    pub fn forget(&mut self, folder: &str) {
        self.remove(folder);
        let _ = self.release(folder);
    }

    /// Settings → Folders Recall manages → remove: its clips stay listed but count as imported
    /// (Delete only removes them from Recall). `Ok(true)` when it was managed. Refused for a
    /// folder that holds or lies in the recordings folder: it would turn into a link over
    /// Recall's own clips (D:\Clips released after recording moved to D:\Clips\Recall).
    pub fn release(&mut self, folder: &str) -> Result<bool, String> {
        let recordings = crate::backend::library_dir_from(self);
        if crate::services::storage::guard::folders_overlap(Path::new(folder), &recordings) {
            return Err(format!("{folder} overlaps the recordings folder {}, so Recall keeps managing it.", recordings.display()));
        }
        let key = folder_key(folder);
        let before = self.former_recordings.len();
        self.former_recordings.retain(|f| folder_key(f) != key);
        Ok(self.former_recordings.len() != before)
    }

    /// Linked sources appear separately from former recordings folders that Recall manages.
    pub(crate) fn linked(&self) -> impl Iterator<Item = &str> {
        self.imported.iter().map(String::as_str).filter(|path| {
            let key = folder_key(path);
            !self.former_recordings.iter().any(|former| folder_key(former) == key)
        })
    }

    /// The former recordings folders Settings lists as "Folders Recall manages" (the current
    /// recordings folder has its own row).
    pub fn managed(&self, recordings: &str) -> Vec<String> {
        let current = folder_key(recordings);
        self.former_recordings.iter().filter(|f| folder_key(f) != current).cloned().collect()
    }

    /// The recordings folder becomes `new`; `old`, the folder it pointed at, stays Recall's own.
    /// Every recordings change goes through here (`change_recordings`, `sync_recordings_folder`,
    /// later `folders_sync::retarget` once per old folder), so no change forgets to retire one.
    pub fn move_recordings(&mut self, new: &str, old: Option<&str>) {
        if let Some(old) = old.filter(|old| folder_key(old) != folder_key(new)) {
            self.retire_recordings(old);
        }
        self.recordings = Some(new.to_owned());
    }

    /// Remembers `folder` as a former recordings folder (once).
    pub fn retire_recordings(&mut self, folder: &str) {
        let key = folder_key(folder);
        if !folder.trim().is_empty() && !self.former_recordings.iter().any(|f| folder_key(f) == key) {
            self.former_recordings.push(folder.to_owned());
        }
    }

    // Tests only: the app changes folders through `folders_sync::retarget`.
    #[cfg(test)]
    /// Settings: new clips save to `new`; `old` joins the imported folders so its clips keep
    /// showing, and stays Recall-owned. True when `old` was added to the list.
    pub fn change_recordings(&mut self, new: &str, old: &str) -> bool {
        self.remove(new);
        self.move_recordings(new, Some(old));
        self.add(Path::new(old))
    }

    /// Settings: adds `picked`; returns the ones that were new. Only those may bring back the
    /// clips removed from Recall under them: a folder already listed (or a parent of one) leaves
    /// removed clips removed.
    pub fn add_all(&mut self, picked: &[PathBuf]) -> Vec<PathBuf> {
        picked.iter().filter(|path| self.add(path)).cloned().collect()
    }
}

impl Folders {
    /// Every folder that holds the user's clips: the library's recordings folder (also
    /// `RECALL_OUT_DIR`, else the default one), the last one the recorder reported, and each
    /// imported folder.
    pub fn clip_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![crate::backend::library_dir_from(self)];
        dirs.extend(self.recordings.iter().chain(&self.imported).map(PathBuf::from));
        dirs
    }
}

/// Start: blocks the cache when a clip folder overlaps it (`storage::guard`). The message
/// to show, if any.
pub fn check_cache_overlap(folders: &Folders) -> Option<String> {
    use crate::services::storage::guard;
    guard::check(&guard::cache_dirs(), &folders.clip_dirs())
}

/// Case-insensitive, separator-normalized, no trailing slash.
/// The comparable form of a path, used wherever ownership, links and folder membership are
/// decided: no verbatim prefix (`\\?\Y:\x` is `Y:\x`, `\\?\UNC\nas\x` is `\\nas\x`; the editor
/// holds a clip by its canonical path, the folder list by the plain one), `\` separators, no
/// trailing `\`, lowercase.
pub fn folder_key(folder: &str) -> String {
    let plain = crate::services::storage::plain_path(Path::new(folder));
    plain.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// True when `path` lies inside `folder` (both plain absolute paths).
pub fn is_inside(path: &str, folder: &str) -> bool {
    let (path, folder) = (folder_key(path), folder_key(folder));
    path.len() > folder.len() && path.starts_with(&folder) && path.as_bytes()[folder.len()] == b'\\'
}

// "Sort clips into game folders": which subfolders are Recall's game folders (`folders_games.rs`).
#[path = "folders_games.rs"]
mod games;
pub use games::{game_folders, in_game_folder, is_recall_named, rule, unbound_game_folders, under_link};

pub fn is_clip(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| CLIP_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Lists clips in an imported folder and its subfolders (depth-first, bounded). Hidden and
/// system folders, and Recall's own cache, are skipped.
pub fn scan_imported(root: &Path, skip: &[PathBuf]) -> Vec<FoundClip> {
    scan_from(root, 0, skip)
}

/// The clips [`scan_imported`] would find inside `folder`, a subfolder of `root`, without
/// walking the rest of `root`.
pub fn scan_subfolder(root: &Path, folder: &Path, skip: &[PathBuf]) -> Vec<FoundClip> {
    if !scan_covers(root, &folder.join("x"), skip) {
        return Vec::new();
    }
    let depth = folder.strip_prefix(root).map_or(0, |rel| rel.components().count());
    scan_from(folder, depth, skip)
}

/// By name alone: [`scan_imported`] of `root` goes as deep as `path` and into its folders.
pub fn scan_reaches(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return false };
    let folders: Vec<_> = rel.parent().map(|p| p.components().collect()).unwrap_or_default();
    folders.len() < MAX_DEPTH && !folders.iter().any(|f| skipped_name(&f.as_os_str().to_string_lossy()))
}

/// [`scan_reaches`], plus none of `path`'s folders under `root` is hidden or in `skip`.
pub fn scan_covers(root: &Path, path: &Path, skip: &[PathBuf]) -> bool {
    scan_reaches(root, path)
        && path.ancestors().skip(1).take_while(|folder| *folder != root).all(|folder| {
            !skip.iter().any(|s| s == folder) && folder.metadata().map_or(true, |meta| !hidden(&meta))
        })
}

fn scan_from(start: &Path, depth: usize, skip: &[PathBuf]) -> Vec<FoundClip> {
    scan_from_checked(start, depth, skip).0
}

/// The walk, and whether it saw everything: false when a folder (the start included) could not
/// be read, an entry's metadata failed, or a size limit cut it short. Only a complete walk may
/// prove a clip is gone.
fn scan_from_checked(start: &Path, depth: usize, skip: &[PathBuf]) -> (Vec<FoundClip>, bool) {
    let mut found = Vec::new();
    let mut walked = 0usize;
    let mut stack = vec![(start.to_path_buf(), depth)];
    let mut complete = true;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            complete = false;
            continue;
        };
        for item in listing {
            let Ok(item) = item else {
                complete = false;
                continue;
            };
            walked += 1;
            if walked > MAX_ENTRIES {
                return (found, false);
            }
            let Ok(meta) = item.metadata() else {
                complete = false;
                continue;
            };
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
            let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            found.push(FoundClip { path, bytes: meta.len(), mtime, folder });
            if found.len() >= MAX_FILES {
                return (found, false);
            }
        }
    }
    (found, complete)
}

/// [`scan_imported`] with a per-folder cache (refreshed after [`SCAN_TTL`]), so listing the
/// library after every save does not re-walk large imported trees.
pub fn scan_imported_cached(root: &Path, skip: &[PathBuf]) -> Vec<FoundClip> {
    scan_imported_cached_checked(root, skip).0
}

/// [`scan_imported_cached`], and whether the walk saw the whole folder (see `scan_from_checked`).
pub fn scan_imported_cached_checked(root: &Path, skip: &[PathBuf]) -> (Vec<FoundClip>, bool) {
    use std::{collections::HashMap, time::Instant};
    let key = PathBuf::from(folder_key(&root.to_string_lossy()));
    if let Some((at, clips, complete)) = CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|m| m.get(&key)).cloned()) {
        if at.elapsed() < SCAN_TTL {
            return (clips, complete);
        }
    }
    let (clips, complete) = scan_from_checked(root, 0, skip);
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(key, (Instant::now(), clips.clone(), complete));
    }
    (clips, complete)
}

type ScanCache = std::collections::HashMap<PathBuf, (std::time::Instant, Vec<FoundClip>, bool)>;
static CACHE: std::sync::Mutex<Option<ScanCache>> = std::sync::Mutex::new(None);

/// Drops cached folder walks (after a delete, or when the folder list changes).
pub fn forget_scans() {
    if let Ok(mut cache) = CACHE.lock() {
        *cache = None;
    }
}

/// Drops one folder's cached walk.
pub fn forget_scan(root: &Path) {
    if let Ok(mut cache) = CACHE.lock() {
        if let Some(scans) = cache.as_mut() {
            scans.remove(Path::new(&folder_key(&root.to_string_lossy())));
        }
    }
}

/// Brings cached walks up to date with what the watcher saw, so the next listing neither
/// drops a new clip nor shows a gone one, and nothing is re-walked for it.
pub fn patch_scans(found: &[FoundClip], gone: &[String], gone_under: &[String]) {
    let Ok(mut cache) = CACHE.lock() else { return };
    let Some(scans) = cache.as_mut() else { return };
    let mut leaving: std::collections::HashSet<String> = gone.iter().map(|p| folder_key(p)).collect();
    leaving.extend(found.iter().map(|f| folder_key(&f.path.to_string_lossy())));
    for (root, (_, clips, _)) in scans.iter_mut() {
        let root = root.to_string_lossy();
        clips.retain(|clip| {
            let path = clip.path.to_string_lossy();
            !leaving.contains(&folder_key(&path)) && !gone_under.iter().any(|folder| is_inside(&path, folder))
        });
        clips.extend(found.iter().filter(|f| is_inside(&f.path.to_string_lossy(), &root)).cloned());
    }
}

fn hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

/// Recorder bookkeeping folders that never hold clips worth listing: thumbnails, captures still
/// in progress (Overwolf `temp-capture`), and editor output (Medal `editor`, `Edits`, `projects`).
fn skipped_name(name: &str) -> bool {
    let name = name.to_lowercase();
    matches!(name.as_str(), "thumbs" | "thumbnails" | "temp-capture" | "editor" | "edits" | "projects" | "$recycle.bin" | "system volume information" | "windows" | "program files" | "program files (x86)" | "node_modules") || name.starts_with('.')
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
        let mut folders = Folders { recordings: Some(r"E:\Clips".into()), ..Folders::default() };
        assert!(!folders.add(Path::new(r"e:\clips\")));
        assert!(folders.add(Path::new(r"D:\ShadowPlay")));
        assert!(!folders.add(Path::new("d:/shadowplay/")));
        assert_eq!(folders.imported, vec![r"D:\ShadowPlay".to_owned()]);
        folders.remove(r"d:\SHADOWPLAY");
        assert!(folders.imported.is_empty());
    }

    #[test]
    fn adding_a_listed_folder_or_its_parent_brings_nothing_back() {
        let mut folders = Folders { recordings: Some(r"E:\Clips".into()), imported: vec![r"D:\Medal\Clips".into()], ..Folders::default() };
        let picked = [PathBuf::from(r"d:\medal\clips"), PathBuf::from(r"E:\Clips")];
        assert!(folders.add_all(&picked).is_empty(), "already listed: no folder is new, so no clip is un-hidden");
        assert_eq!(folders.add_all(&[PathBuf::from(r"D:\ShadowPlay")]), [PathBuf::from(r"D:\ShadowPlay")]);
    }

    #[test]
    fn a_changed_recordings_folder_is_remembered_as_recall_owned() {
        let mut folders = Folders { recordings: Some(r"E:\Clips".into()), ..Folders::default() };
        assert!(folders.change_recordings(r"F:\New", r"E:\Clips"));
        assert_eq!(folders.recordings.as_deref(), Some(r"F:\New"));
        assert_eq!(folders.imported, [r"E:\Clips".to_owned()]);
        assert_eq!(folders.former_recordings, [r"E:\Clips".to_owned()]);
        // Back again: the new folder leaves the imported list; each former folder is listed once.
        folders.change_recordings(r"E:\Clips", r"F:\New");
        folders.retire_recordings(r"e:\clips\");
        assert_eq!(folders.imported, [r"F:\New".to_owned()]);
        assert_eq!(folders.former_recordings, [r"E:\Clips".to_owned(), r"F:\New".to_owned()]);
        let recordings = Path::new(r"E:\Clips");
        assert!(!super::super::imports::is_imported(r"F:\New\VALORANT__2026-11-01__10-00-00.mp4", recordings, &folders.imported, &folders.former_recordings));
    }

    #[test]
    fn a_managed_folder_can_be_released_or_forgotten() {
        let mut folders = Folders { recordings: Some(r"E:\Clips".into()), ..Folders::default() };
        folders.change_recordings(r"F:\New", r"E:\Clips");
        folders.move_recordings(r"G:\Third", Some(r"F:\New"));
        assert_eq!(folders.managed(r"G:\Third"), [r"E:\Clips".to_owned(), r"F:\New".to_owned()], "shown, not hidden");
        let recordings = Path::new(r"G:\Third");
        let clip = r"E:\Clips\VALORANT__2026-11-01__10-00-00.mp4";
        // Removing it from "Folders Recall manages": still listed, but imported now.
        assert!(folders.release(r"e:\clips\").unwrap());
        assert_eq!(folders.imported, [r"E:\Clips".to_owned()]);
        assert!(super::super::imports::is_imported(clip, recordings, &folders.imported, &folders.former_recordings));
        // Removing a folder from Clip folders drops both.
        folders.change_recordings(r"H:\Fourth", r"G:\Third");
        folders.forget(r"G:\Third");
        assert!(!folders.imported.iter().any(|f| f == r"G:\Third") && !folders.former_recordings.iter().any(|f| f == r"G:\Third"));
        assert_eq!(folders.managed(r"H:\Fourth"), [r"F:\New".to_owned()]);
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
        let folders = Folders { recordings: Some(r"E:\Clips".into()), imported: vec![r"D:\Old".into()], former_recordings: vec![r"D:\Old".into()], ..Folders::default() };
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
        assert_eq!(by_name("loose.mp4").folder.as_deref(), dir.file_name().and_then(|n| n.to_str()));
        assert_eq!(by_name("goal.MKV").folder.as_deref(), Some("Rocket League"));
        assert_eq!(by_name("goal.MKV").bytes, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

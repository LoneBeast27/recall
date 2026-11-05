//! Link table for clips in imported folders (compatibility mode): Recall lists them where the old
//! recorder left them and never moves, renames or writes into those folders. What Recall knows
//! about each file lives on C: instead, at `%APPDATA%\Recall\imports.json`:
//! size + mtime, what [`edit_marks`](super::edit_marks) found, and whether the user removed it
//! from Recall. Editor saves for these clips go to `%APPDATA%\Recall\imports\edits\`.
//! `RECALL_IMPORTS_DIR` replaces `%APPDATA%\Recall` (tests).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::edit_marks::Verdict;
use super::folders::{folder_key, is_inside};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Unknown,
    Clip,
    Edited,
    NoVideo,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Link {
    pub path: String,
    pub bytes: u64,
    pub mtime: u64,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// Removed from Recall by the user; the file stays where it is.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Table {
    version: u32,
    files: BTreeMap<String, Link>,
}

/// What the library does with one imported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// A checked recording.
    Show,
    /// Not checked yet, or changed since: list it and queue a check.
    Check,
    /// An editor's export, no video, or removed by the user.
    Skip,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

fn base_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RECALL_IMPORTS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(test) {
        #[cfg(test)]
        crate::test_temp::sweep_dead("recall-imports-test");
        return Some(std::env::temp_dir().join(format!("recall-imports-test-{}", std::process::id())));
    }
    std::env::var_os("APPDATA").filter(|d| !d.is_empty()).map(|root| PathBuf::from(root).join("Recall"))
}

fn table_path() -> Option<PathBuf> {
    base_dir().map(|dir| dir.join("imports.json"))
}

/// Where editor saves for imported clips go.
pub fn edits_dir() -> Option<PathBuf> {
    base_dir().map(|dir| dir.join("imports").join("edits"))
}

/// The editor save for an imported clip: `<edits>\<path hash>~<file name>.recall-edit.json`.
/// The hash keeps two `clip.mp4` in different folders apart; the name keeps it readable.
pub fn edit_file(source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?.to_string_lossy();
    Some(edits_dir()?.join(format!("{:016x}~{name}.recall-edit.json", path_hash(&source.to_string_lossy()))))
}

/// FNV-1a over the folder key: stable across runs and Rust versions.
fn path_hash(path: &str) -> u64 {
    folder_key(path).bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The clip file name an edit file belongs to (`<hash>~` removed).
pub fn edit_file_clip_name(edit_name: &str) -> &str {
    match edit_name.split_once('~') {
        Some((hash, rest)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => rest,
        _ => edit_name,
    }
}

/// True when `path` is inside one of `imported` and not directly in the recordings folder.
pub fn is_imported(path: &str, recordings: &Path, imported: &[String]) -> bool {
    let parent = Path::new(path).parent().map(|p| folder_key(&p.to_string_lossy())).unwrap_or_default();
    parent != folder_key(&recordings.to_string_lossy()) && imported.iter().any(|folder| is_inside(path, folder))
}

fn with_table<T>(work: impl FnOnce(&mut Table) -> T) -> T {
    let mut guard = TABLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let table = guard.get_or_insert_with(|| table_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    work(table)
}

pub fn status(path: &Path, bytes: u64, mtime: u64) -> Status {
    let key = folder_key(&path.to_string_lossy());
    with_table(|table| match table.files.get(&key) {
        Some(link) if link.hidden => Status::Skip,
        Some(link) if link.bytes != bytes || link.mtime != mtime => Status::Check,
        Some(link) => match link.kind {
            Kind::Clip => Status::Show,
            Kind::Edited | Kind::NoVideo => Status::Skip,
            Kind::Unknown => Status::Check,
        },
        None => Status::Check,
    })
}

/// Stores what a check found (an `Unready` file is left to the next check). True when the file
/// is now skipped.
pub fn record(path: &Path, bytes: u64, mtime: u64, verdict: &Verdict) -> bool {
    let (kind, editor) = match verdict {
        Verdict::Clip => (Kind::Clip, None),
        Verdict::Edited(name) => (Kind::Edited, Some((*name).to_owned())),
        Verdict::NoVideo => (Kind::NoVideo, None),
        Verdict::Unready => return false,
    };
    let text = path.to_string_lossy().into_owned();
    with_table(|table| {
        let link = table.files.entry(folder_key(&text)).or_default();
        let hidden = link.hidden;
        *link = Link { path: text, bytes, mtime, kind, editor, hidden };
        hidden || kind != Kind::Clip
    })
}

/// Removes clips from Recall without touching the files. Returns how many.
pub fn hide(paths: &[PathBuf]) -> Result<usize, String> {
    with_table(|table| {
        for path in paths {
            let text = path.to_string_lossy().into_owned();
            let meta = path.metadata().ok();
            let link = table.files.entry(folder_key(&text)).or_insert_with(|| Link {
                path: text.clone(),
                bytes: meta.as_ref().map_or(0, |m| m.len()),
                mtime: meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
                ..Link::default()
            });
            link.hidden = true;
        }
    });
    save()?;
    Ok(paths.len())
}

/// Shows the hidden clips under `folder` again (the folder was added back).
pub fn unhide_under(folder: &str) -> Result<(), String> {
    let changed = with_table(|table| {
        let mut changed = false;
        for link in table.files.values_mut().filter(|link| link.hidden && is_inside(&link.path, folder)) {
            link.hidden = false;
            changed = true;
        }
        changed
    });
    if changed { save() } else { Ok(()) }
}

/// How many files under `folder` were skipped as an editor's export.
pub fn skipped_under(folder: &str) -> usize {
    with_table(|table| table.files.values().filter(|l| l.kind == Kind::Edited && !l.hidden && is_inside(&l.path, folder)).count())
}

/// Atomic: `imports.json.tmp`, then rename over the file.
pub fn save() -> Result<(), String> {
    let path = table_path().ok_or("APPDATA is not set")?;
    let bytes = with_table(|table| {
        table.version = 1;
        serde_json::to_vec(table).map_err(|e| e.to_string())
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("write imports: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save imports: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_checked_once_and_again_when_it_changes() {
        let path = Path::new(r"Z:\recall-imports-test\once\a.mp4");
        assert_eq!(status(path, 10, 20), Status::Check);
        assert!(!record(path, 10, 20, &Verdict::Clip));
        assert_eq!(status(path, 10, 20), Status::Show);
        assert_eq!(status(path, 11, 20), Status::Check);
        assert!(!record(path, 10, 20, &Verdict::Unready), "an unready look stores nothing");
        assert!(record(path, 11, 20, &Verdict::Edited("Adobe Premiere Pro")));
        assert_eq!(status(path, 11, 20), Status::Skip);
        assert_eq!(skipped_under(r"Z:\recall-imports-test\once"), 1);
    }

    #[test]
    fn hiding_survives_a_recheck_and_re_adding_the_folder_undoes_it() {
        let path = PathBuf::from(r"Z:\recall-imports-test\hide\b.mp4");
        record(&path, 5, 6, &Verdict::Clip);
        hide(std::slice::from_ref(&path)).unwrap();
        assert_eq!(status(&path, 5, 6), Status::Skip);
        assert!(record(&path, 5, 7, &Verdict::Clip), "a changed file stays hidden");
        unhide_under(r"Z:\recall-imports-test\hide").unwrap();
        assert_eq!(status(&path, 5, 7), Status::Show);
        assert!(table_path().unwrap().is_file());
    }

    #[test]
    fn edit_files_keep_same_named_clips_apart() {
        let a = edit_file(Path::new(r"D:\Old\Valorant\clip.mp4")).unwrap();
        let b = edit_file(Path::new(r"D:\Old\Apex\clip.mp4")).unwrap();
        assert_ne!(a, b);
        assert_eq!(a, edit_file(Path::new(r"d:/old/valorant/clip.mp4")).unwrap(), "case and slashes do not matter");
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(edit_file_clip_name(&name), "clip.mp4.recall-edit.json");
        assert_eq!(edit_file_clip_name("a~b.recall-edit.json"), "a~b.recall-edit.json");
    }

    #[test]
    fn recordings_folder_clips_are_never_imported() {
        let imported = vec![r"V:\".to_owned(), r"D:\Old".to_owned()];
        assert!(!is_imported(r"V:\Recall\Raw\a.mp4", Path::new(r"V:\Recall\Raw"), &imported));
        assert!(is_imported(r"V:\Other\a.mp4", Path::new(r"V:\Recall\Raw"), &imported));
        assert!(is_imported(r"D:\Old\Game\a.mp4", Path::new(r"V:\Recall\Raw"), &imported));
        assert!(!is_imported(r"E:\a.mp4", Path::new(r"V:\Recall\Raw"), &imported));
    }
}

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
pub(crate) struct Table {
    version: u32,
    files: BTreeMap<String, Link>,
    /// Why saving is refused: the file exists but could not be read this run.
    #[serde(skip)]
    write_block: Option<String>,
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

pub(crate) fn base_dir() -> Option<PathBuf> {
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
#[cfg(test)]
pub fn edit_file_clip_name(edit_name: &str) -> &str {
    match edit_name.split_once('~') {
        Some((hash, rest)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => rest,
        _ => edit_name,
    }
}

/// True when `path` is inside one of `imported` and not directly in the recordings folder or a
/// folder that was the recordings folder before (`former`: those clips stay Recall's own, so
/// Delete recycles them and their edits stay beside them).
pub fn is_imported(path: &str, recordings: &Path, imported: &[String], former: &[String]) -> bool {
    !is_owned(Path::new(path), recordings, former) && imported.iter().any(|folder| is_inside(path, folder))
}

/// Only clips saved directly into a current or former recordings folder, or listed by exact path
/// as recorded by Recall elsewhere (`recorded`), are managed.
pub(crate) fn is_owned(clip: &Path, recordings: &Path, former: &[String]) -> bool {
    let Some(parent) = clip.parent() else { return false };
    let parent = folder_key(&parent.to_string_lossy());
    !parent.is_empty() && (parent == folder_key(&recordings.to_string_lossy())
        || former.iter().any(|folder| folder_key(folder) == parent))
        // A clip Recall recorded into a startup `-OutDir` folder: that exact file only.
        || super::recorded::contains(clip)
}

fn with_table<T>(work: impl FnOnce(&mut Table) -> T) -> T {
    let mut guard = TABLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let table = guard.get_or_insert_with(|| table_path().map(|p| load_table(&p)).unwrap_or_default());
    work(table)
}

/// The table at `path`, entry by entry (one entry this build can't read drops only
/// itself). Missing -> empty. Damaged -> kept as a dated `.bad` first (moved when nothing
/// parsed, copied when only some entries did). Unreadable -> empty, and saving is refused.
pub(crate) fn load_table(path: &Path) -> Table {
    use crate::services::durable::{quarantine, Keep};
    crate::services::durable::sweep_temps(path, std::time::SystemTime::now());
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Table::default(),
        Err(error) => return Table { write_block: Some(format!("imports.json could not be read ({error}); not saved")), ..Table::default() },
    };
    match parse_table(&bytes) {
        Some((table, false)) => table,
        Some((table, true)) => {
            quarantine(path, std::time::SystemTime::now(), Keep::Copy);
            table
        }
        None => {
            quarantine(path, std::time::SystemTime::now(), Keep::Move);
            Table::default()
        }
    }
}

/// The table and whether any part was dropped; None when `bytes` is not a JSON object.
fn parse_table(bytes: &[u8]) -> Option<(Table, bool)> {
    let serde_json::Value::Object(root) = serde_json::from_slice(bytes).ok()? else { return None };
    let mut lossy = false;
    let version = match root.get("version") {
        Some(v) => v.as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or_else(|| {
            lossy = true;
            0
        }),
        None => 0,
    };
    let mut files = BTreeMap::new();
    match root.get("files") {
        Some(serde_json::Value::Object(map)) => {
            for (key, value) in map {
                match serde_json::from_value::<Link>(value.clone()) {
                    Ok(link) => {
                        files.insert(key.clone(), link);
                    }
                    Err(_) => lossy = true,
                }
            }
        }
        Some(_) => lossy = true,
        None => {}
    }
    Some((Table { version, files, write_block: None }, lossy))
}

/// Whether the table hides the file with folder key `key` (tests).
#[cfg(test)]
pub(crate) fn table_hides(table: &Table, key: &str) -> bool {
    table.files.get(key).is_some_and(|link| link.hidden)
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
        Verdict::Unready | Verdict::Unreadable => return false,
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

/// Durable: a temp file, flushed, then renamed over the file. The table is serialized
/// under its lock and written after it is released, so `status` callers never wait on
/// the drive; the ticket keeps an older snapshot from landing over a newer one.
pub fn save() -> Result<(), String> {
    let path = table_path().ok_or("APPDATA is not set")?;
    let (ticket, bytes) = with_table(|table| {
        table.version = 1;
        snapshot(table).map(|bytes| (WRITER.ticket(), bytes))
    })?;
    write_snapshot(&path, ticket, &bytes)
}

static WRITER: std::sync::LazyLock<crate::services::ordered_write::OrderedWriter> = std::sync::LazyLock::new(Default::default);

/// `table` as JSON; refused after a failed read.
fn snapshot(table: &Table) -> Result<Vec<u8>, String> {
    if let Some(reason) = &table.write_block {
        return Err(reason.clone());
    }
    serde_json::to_vec(table).map_err(|e| e.to_string())
}

fn write_snapshot(path: &Path, ticket: u64, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    WRITER.write(ticket, path, bytes).map_err(|e| format!("save imports: {e}"))
}

/// Writes `table` to `path` (temp file, flushed, then renamed); refused after a failed read.
#[cfg(test)]
pub(crate) fn save_table(path: &Path, table: &Table) -> Result<(), String> {
    let bytes = snapshot(table)?;
    crate::services::durable::write_durable(path, &bytes).map_err(|e| format!("save imports: {e}"))
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
        assert!(!is_imported(r"V:\Recall\Raw\a.mp4", Path::new(r"V:\Recall\Raw"), &imported, &[]));
        assert!(is_imported(r"V:\Other\a.mp4", Path::new(r"V:\Recall\Raw"), &imported, &[]));
        assert!(is_imported(r"D:\Old\Game\a.mp4", Path::new(r"V:\Recall\Raw"), &imported, &[]));
        assert!(!is_imported(r"E:\a.mp4", Path::new(r"V:\Recall\Raw"), &imported, &[]));
    }

    #[test]
    fn a_former_recordings_folder_stays_recall_owned() {
        let imported = vec![r"D:\OldRecall".to_owned(), r"D:\Medal".to_owned()];
        let former = vec![r"d:\oldrecall\".to_owned()];
        assert!(!is_imported(r"D:\OldRecall\VALORANT__2026-10-01__10-00-00.mp4", Path::new(r"V:\Recall"), &imported, &former));
        assert!(is_imported(r"D:\OldRecall\sub\x.mp4", Path::new(r"V:\Recall"), &imported, &former), "only its own top level, like the recordings folder");
        assert!(is_imported(r"D:\Medal\x.mp4", Path::new(r"V:\Recall"), &imported, &former));
    }
}

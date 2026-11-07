//! Background check of imported files nobody has looked at yet: one thread, one file at a time,
//! on the file's hard-drive lane at background priority. The library lists these files at once;
//! the ones that turn out to be an editor's export (or have no video) are handed to the sink,
//! which drops them from the library.
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::{edit_marks, folders::FoundClip, imports};
use crate::services::storage::{self, Priority};

type Sink = Box<dyn Fn(Vec<PathBuf>) + Send + Sync>;

/// Files waiting: (path, bytes, mtime).
static QUEUE: Mutex<Vec<(PathBuf, u64, u64)>> = Mutex::new(Vec::new());
static RUNNING: Mutex<bool> = Mutex::new(false);
static SINK: OnceLock<Sink> = OnceLock::new();

/// Skipped files go here (set once, by the worker).
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

/// Queues files for a check and starts the checker when it is idle.
pub fn queue(files: Vec<(PathBuf, u64, u64)>) {
    if files.is_empty() {
        return;
    }
    {
        let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
        for file in files {
            if !queue.iter().any(|(path, ..)| path == &file.0) {
                queue.push(file);
            }
        }
    }
    let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if !*running {
        *running = true;
        if std::thread::Builder::new().name("recall-import-check".into()).spawn(run).is_err() {
            *running = false;
        }
    }
}

/// The imported clips a listing shows: checked recordings, plus files not checked yet (queued
/// for a check). Editors' exports, files without video and clips the user removed are left out.
/// A clip directly in the recordings folder (an imported folder may hold it) is Recall's own.
pub fn listable(found: Vec<FoundClip>, recordings: &Path, imported: &[String], former: &[String]) -> Vec<FoundClip> {
    let mut pending = Vec::new();
    let kept = found
        .into_iter()
        .filter(|clip| {
            if !imports::is_imported(&clip.path.to_string_lossy(), recordings, imported, former) {
                return true;
            }
            match imports::status(&clip.path, clip.bytes, clip.mtime) {
                imports::Status::Show => true,
                imports::Status::Skip => false,
                imports::Status::Check => {
                    pending.push((clip.path.clone(), clip.bytes, clip.mtime));
                    true
                }
            }
        })
        .collect();
    queue(pending);
    kept
}

/// How many files a batch checks before it hands skips over and saves the table.
const BATCH: usize = 25;

fn run() {
    let mut checked = 0usize;
    loop {
        let batch: Vec<_> = {
            let mut queue = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
            let take = queue.len().min(BATCH);
            queue.drain(..take).collect()
        };
        if batch.is_empty() {
            let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
            // queue() pushes before it looks at RUNNING, so a file queued now is seen here.
            if QUEUE.lock().map_or(true, |q| q.is_empty()) {
                *running = false;
                break;
            }
            continue;
        }
        let skipped = check_batch(batch);
        checked += BATCH;
        if !skipped.is_empty() {
            if let Some(sink) = SINK.get() {
                sink(skipped);
            }
        }
        if checked % (BATCH * 8) == 0 {
            let _ = imports::save();
        }
    }
    let _ = imports::save();
}

/// Checks each file still waiting for one; returns the ones now skipped.
pub(crate) fn check_batch(batch: Vec<(PathBuf, u64, u64)>) -> Vec<PathBuf> {
    let mut skipped = Vec::new();
    for (path, bytes, mtime) in batch {
        if imports::status(&path, bytes, mtime) != imports::Status::Check {
            continue;
        }
        let file = path.clone();
        let mut verdict = storage::run_io(&path, Priority::Background, move || edit_marks::inspect(&file)).unwrap_or(edit_marks::Verdict::Unreadable);
        // No index long after the last write: a recording that never finished (6 such files on
        // Rohan's V:, all `ftyp free mdat` and zeros). It cannot play; stop looking at it. A file
        // that could not be opened at all (`Unreadable`) is only looked at again.
        if verdict == edit_marks::Verdict::Unready && settled(mtime) && path.metadata().is_ok_and(|m| m.len() == bytes) {
            verdict = edit_marks::Verdict::NoVideo;
        }
        if imports::record(&path, bytes, mtime, &verdict) {
            skipped.push(path);
        }
    }
    skipped
}

/// Written more than 10 minutes ago: no recorder is still filling it.
fn settled(mtime: u64) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    now.saturating_sub(mtime) > 600
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn an_old_clip_that_cannot_be_opened_once_is_not_stored_as_no_video() {
        let dir = std::env::temp_dir().join(format!("recall-import-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("locked.mp4");
        std::fs::write(&path, b"not an mp4 header").unwrap();
        let bytes = path.metadata().unwrap().len();
        let old = 1_600_000_000; // long settled
        // Held with no sharing: opening fails, the size still reads.
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        assert!(check_batch(vec![(path.clone(), bytes, old)]).is_empty());
        assert_eq!(imports::status(&path, bytes, old), imports::Status::Check, "looked at again next listing");
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

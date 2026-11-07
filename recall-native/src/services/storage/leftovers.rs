//! Leftover kept copies: the recorder's start-up check (`recall/src/encoder/staging_kept.rs`)
//! sets a kept copy that matches no whole clip aside in `staging\kept\leaked\` (its clip has
//! another size, or was deleted). Such a copy may be the only whole version of a clip (a
//! damaged V: file has another size), so nothing empties the folder on its own: Settings ->
//! Storage shows it, opens it, and sends it to the Recycle Bin on the user's click, through the
//! same Recycle Bin path as a library Delete (room precheck under the shared bin lock,
//! IFileOperation on a private desktop, never a permanent delete).
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// `kept\leaked`, as the recorder names it.
const LEAKED_DIR: &str = "leaked";

/// The leftover copies in one folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Leftovers {
    pub dir: PathBuf,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

/// Moves files to the Recycle Bin. Production: [`RECYCLE`].
pub(super) type RecycleFn = fn(&[PathBuf]) -> Result<usize, String>;

/// The library Delete's Recycle Bin path (`services/library/win.rs recycle`).
pub(super) const RECYCLE: RecycleFn = crate::services::library::recycle_to_bin;

/// The clip files directly in `dir`.
pub(super) fn leftovers_in(dir: &Path) -> Leftovers {
    let mut found = Leftovers { dir: dir.to_path_buf(), ..Leftovers::default() };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let is_clip = entry.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".mp4");
        if let Some(meta) = entry.metadata().ok().filter(|m| m.is_file() && is_clip) {
            found.bytes += meta.len();
            found.files.push(entry.path());
        }
    }
    found.files.sort();
    found
}

/// The leftover copies of the kept folders (the engine's beside the exe, and the cache's own),
/// or None when there are none.
pub fn leftovers() -> Option<Leftovers> {
    let mut seen = Vec::new();
    let mut all: Option<Leftovers> = None;
    for dir in super::kept::kept_dirs().into_iter().map(|d| d.join(LEAKED_DIR)) {
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let found = leftovers_in(&dir);
        if found.files.is_empty() {
            continue;
        }
        match &mut all {
            Some(all) => {
                all.bytes += found.bytes;
                all.files.extend(found.files);
            }
            None => all = Some(found),
        }
    }
    all
}

/// Settings -> Storage -> Send to Recycle Bin. Returns how many copies went.
pub fn send_to_bin() -> Result<usize, String> {
    send_in(leftovers(), RECYCLE)
}

pub(super) fn send_in(found: Option<Leftovers>, recycle: RecycleFn) -> Result<usize, String> {
    match found {
        Some(found) if !found.files.is_empty() => recycle(&found.files),
        _ => Ok(0),
    }
}

static LOGGED: AtomicBool = AtomicBool::new(false);

/// Logs the leftover copies once per start (from the start-up kept sweep).
pub(super) fn log_once() {
    if LOGGED.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(found) = leftovers() {
        eprintln!("kept clips: {} leftover cop(ies), {} MiB, in {} (kept until sent to the Recycle Bin in Settings)", found.files.len(), found.bytes >> 20, found.dir.display());
    }
}

/// "2 leftover copies (3.5 GB) on C:" for Settings, None when there are none.
pub fn leftover_line(count: usize, bytes: u64, drive: &str) -> Option<String> {
    (count > 0).then(|| {
        let noun = if count == 1 { "copy" } else { "copies" };
        let gb = bytes as f64 / 1_073_741_824.0;
        format!("{count} leftover {noun} ({gb:.1} GB) on {drive}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftovers_are_listed_and_only_go_through_the_library_recycle_path() {
        let dir = std::env::temp_dir().join(format!("recall-leftovers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.mp4"), vec![1u8; 300]).unwrap();
        std::fs::write(dir.join("B.mp4"), vec![1u8; 200]).unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let found = leftovers_in(&dir);
        assert_eq!((found.files.len(), found.bytes), (2, 500), "{found:?}");
        // The production action is the library Delete's Recycle Bin call, nothing else.
        assert!(std::ptr::fn_addr_eq(RECYCLE, crate::services::library::recycle_to_bin as RecycleFn));
        static SENT: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
        let fake: RecycleFn = |files| {
            SENT.lock().unwrap().extend(files.iter().cloned());
            Ok(files.len())
        };
        assert_eq!(send_in(Some(found.clone()), fake), Ok(2));
        assert_eq!(*SENT.lock().unwrap(), found.files, "exactly the listed copies");
        assert_eq!(send_in(None, fake), Ok(0), "nothing to send");
        assert!(dir.join("A.mp4").exists(), "the test's fake bin moved nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_names_the_leftover_copies() {
        assert_eq!(leftover_line(0, 0, "C:"), None);
        assert_eq!(leftover_line(1, 1_073_741_824, "C:").as_deref(), Some("1 leftover copy (1.0 GB) on C:"));
        assert_eq!(leftover_line(3, 3_758_096_384, "C:").as_deref(), Some("3 leftover copies (3.5 GB) on C:"));
    }
}

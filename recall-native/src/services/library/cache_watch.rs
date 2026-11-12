//! Keeps the sidebar's SSD row honest when files the row counts come or go: the cache folder,
//! and `Edited` when it shares the cache's drive. A removal (another program: WizTree,
//! Explorer) and a new file (Recall's own thumbnails, copies and heads, which do not mark the
//! totals stale) both ask for a fresh walk, which the row takes at most every few seconds
//! (`ssd_meter`); with no change it no longer walks once a minute. Temporary files (`.part`,
//! `.tmp`) are ignored: a copy counts once it is renamed into place. Every removal also moves
//! [`removals`], so the library re-checks the thumbnails on screen (`queue_visible`). The
//! threads sleep in the OS until something changes. Nothing here writes or deletes anything.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use super::watch_win::{self, Action, Raw, Stop};
use super::watch::WatchRoot;

static REMOVALS: AtomicU64 = AtomicU64::new(0);

/// Moves on with every burst of removals in any watched folder (the library compares it).
pub fn removals() -> u64 {
    REMOVALS.load(Ordering::Acquire)
}

/// What one burst of notifications changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Burst {
    removed: bool,
    added: bool,
}

/// Starts watching `dirs` (recursively) for the life of the process; `on_removed` runs once per
/// burst with removals, `on_added` once per burst with new files.
pub fn start(dirs: Vec<PathBuf>, on_removed: impl Fn() + Send + 'static, on_added: impl Fn() + Send + 'static) {
    let (tx, rx) = mpsc::channel::<Raw>();
    for dir in dirs {
        let Some(stop) = Stop::new().map(Arc::new) else { continue };
        let (tx, root) = (tx.clone(), WatchRoot { path: dir, recursive: true });
        // Nobody waits for the first arm: the next walk counts anything removed before it.
        let (armed, _) = mpsc::channel();
        let _ = thread::Builder::new().name("recall-cache-watch".into()).spawn(move || watch_win::watch_root(root, stop, tx, armed));
    }
    drop(tx);
    let _ = thread::Builder::new().name("recall-cache-watch-settle".into()).spawn(move || {
        while let Ok(raw) = rx.recv() {
            let mut burst = counts(&raw);
            while let Ok(more) = rx.try_recv() {
                let next = counts(&more);
                burst.removed |= next.removed;
                burst.added |= next.added;
            }
            if burst.removed {
                REMOVALS.fetch_add(1, Ordering::AcqRel);
                crate::services::ui_wake::wake();
                on_removed();
            }
            if burst.added {
                on_added();
            }
        }
    });
}

/// What one notification changes in what the SSD row counts. A file growing in place is not
/// counted (copies grow as `.part`; a walk per write would cost more than it shows).
fn counts(raw: &Raw) -> Burst {
    match raw {
        Raw::Change { path, action: Action::Removed, .. } => Burst { removed: !temporary(path), added: false },
        Raw::Change { path, action: Action::Added, .. } => Burst { removed: false, added: !temporary(path) },
        Raw::Change { .. } | Raw::RootsChanged | Raw::Stop => Burst::default(),
        Raw::Overflow(_) | Raw::Lost(_) | Raw::Back(_) => Burst { removed: true, added: false },
    }
}

fn temporary(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.ends_with(".part") || name.ends_with(".tmp")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn new_and_removed_files_ask_for_a_walk_temp_files_do_not() {
        let change = |name: &str, action| Raw::Change { root: PathBuf::from(r"C:\cache"), recursive: true, path: PathBuf::from(r"C:\cache\video").join(name), action };
        let (removed, added) = (Burst { removed: true, added: false }, Burst { removed: false, added: true });
        assert_eq!(counts(&change("abc.mp4", Action::Removed)), removed);
        assert_eq!(counts(&change("abc.mp4.12-34.part", Action::Removed)), Burst::default(), "Recall's own temp file");
        assert_eq!(counts(&change("folders.json.tmp", Action::Removed)), Burst::default());
        assert_eq!(counts(&change("abc.mp4", Action::Added)), added, "a copy renamed into place, a new thumbnail");
        assert_eq!(counts(&change("abc.mp4.12-34.part", Action::Added)), Burst::default(), "a copy starting");
        assert_eq!(counts(&change("abc.mp4", Action::Modified)), Burst::default(), "a file growing: no walk per write");
        assert_eq!(counts(&Raw::Overflow(PathBuf::from(r"C:\cache"))), removed);
    }

    #[test]
    fn a_file_deleted_by_another_program_is_noticed() {
        let dir = crate::test_temp::TempDir::new(format!("recall-cache-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("thumbs")).unwrap();
        let (file, part) = (dir.join("thumbs").join("a.jpg"), dir.join("thumbs").join("b.jpg.1-2.part"));
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(&part, b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        let before = removals();
        start(vec![dir.clone()], move || {
            let _ = tx.send(());
        }, || {});
        thread::sleep(Duration::from_millis(300)); // armed
        std::fs::remove_file(&part).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(700)).is_err(), "a temp file going is Recall's own");
        std::fs::remove_file(&file).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok(), "the removal asks for a walk");
        assert!(removals() > before, "and has the library re-check its thumbnails");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

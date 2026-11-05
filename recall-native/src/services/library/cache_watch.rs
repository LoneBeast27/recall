//! Keeps the sidebar's SSD row honest when another program (WizTree, Explorer) deletes files
//! the row counts: the cache folder, and `Edited` when it shares the cache's drive. A removal
//! asks for a fresh walk, which the row takes at most every few seconds (`ssd_meter`), instead
//! of waiting for its once-a-minute refresh. Recall's own changes already mark the totals stale,
//! so only removals count, and its temporary files (`.part`, `.tmp`) are ignored. The threads
//! sleep in the OS until something changes. Nothing here writes or deletes anything.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

use super::watch_win::{self, Action, Raw, Stop};
use super::watch::WatchRoot;

/// Starts watching `dirs` (recursively) for the life of the process; `on_removed` runs once per
/// burst of removals.
pub fn start(dirs: Vec<PathBuf>, on_removed: impl Fn() + Send + 'static) {
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
            let mut changed = counts(&raw);
            while let Ok(more) = rx.try_recv() {
                changed |= counts(&more);
            }
            if changed {
                on_removed();
            }
        }
    });
}

/// Whether one notification can change what the SSD row counts.
fn counts(raw: &Raw) -> bool {
    match raw {
        Raw::Change { path, action: Action::Removed, .. } => !temporary(path),
        Raw::Change { .. } | Raw::RootsChanged | Raw::Stop => false,
        Raw::Overflow(_) | Raw::Lost(_) | Raw::Back(_) => true,
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
    fn only_outside_removals_ask_for_a_walk() {
        let change = |name: &str, action| Raw::Change { root: PathBuf::from(r"C:\cache"), recursive: true, path: PathBuf::from(r"C:\cache\video").join(name), action };
        assert!(counts(&change("abc.mp4", Action::Removed)));
        assert!(!counts(&change("abc.mp4.12-34.part", Action::Removed)), "Recall's own temp file");
        assert!(!counts(&change("folders.json.tmp", Action::Removed)));
        assert!(!counts(&change("abc.mp4", Action::Added)) && !counts(&change("abc.mp4", Action::Modified)));
        assert!(counts(&Raw::Overflow(PathBuf::from(r"C:\cache"))));
    }

    #[test]
    fn a_file_deleted_by_another_program_is_noticed() {
        let dir = crate::test_temp::TempDir::new(format!("recall-cache-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("thumbs")).unwrap();
        let (file, part) = (dir.join("thumbs").join("a.jpg"), dir.join("thumbs").join("b.jpg.1-2.part"));
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(&part, b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        start(vec![dir.clone()], move || {
            let _ = tx.send(());
        });
        thread::sleep(Duration::from_millis(300)); // armed
        std::fs::remove_file(&part).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(700)).is_err(), "a temp file going is Recall's own");
        std::fs::remove_file(&file).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok(), "the removal asks for a walk");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

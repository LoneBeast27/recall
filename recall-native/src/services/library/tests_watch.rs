//! Folder watcher: scope, notification decoding and the probe, then the real thing on temp folders.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use super::{
    folders,
    settle::{Kind, Probe},
    watch::{ClipChanges, ClipWatcher, LibraryChange, WatchRoot, covering_folder, probe, probe_in},
    watch_win::{Action, parse_notifications},
};

#[test]
fn notifications_decode_chained_and_end_cleanly() {
    fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut bytes = Vec::new();
        bytes.extend(next.to_ne_bytes());
        bytes.extend(action.to_ne_bytes());
        bytes.extend((units.len() as u32 * 2).to_ne_bytes());
        units.iter().for_each(|unit| bytes.extend(unit.to_ne_bytes()));
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        bytes
    }
    let first = record(0, 1, "a.mp4").len() as u32;
    let second = record(0, 4, r"sub\old.mp4").len() as u32;
    let mut data = record(first, 1, "a.mp4");
    data.extend(record(second, 4, r"sub\old.mp4"));
    data.extend(record(0, 5, "né.mp4"));
    let parsed: Vec<_> = parse_notifications(&data).into_iter().map(|(a, n)| (a, n.to_string_lossy().into_owned())).collect();
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0], (Action::Added, "a.mp4".to_owned()));
    assert_eq!(parsed[1], (Action::Removed, r"sub\old.mp4".to_owned()));
    assert_eq!(parsed[2], (Action::Added, "né.mp4".to_owned()));
    assert!(parse_notifications(&data[..3]).is_empty(), "a short buffer is not a crash");
    assert_eq!(parse_notifications(&record(0, 3, "x.mp4"))[0].0, Action::Modified);
    let mut lying = record(0, 1, "a.mp4");
    lying[8] = 200;
    assert!(parse_notifications(&lying).is_empty(), "a name past the buffer is dropped");
}

#[test]
fn scope_matches_what_the_listing_shows() {
    let dir = temp("scope");
    let (recordings, imported) = (dir.join("rec"), dir.join("old"));
    fs::create_dir_all(recordings.join("exports")).unwrap();
    fs::create_dir_all(imported.join("a").join("b").join("c").join("d")).unwrap();
    fs::create_dir_all(imported.join("thumbs")).unwrap();
    let roots = [WatchRoot { path: recordings.clone(), recursive: false }, WatchRoot { path: imported.clone(), recursive: true }];
    let covered = |path: PathBuf| covering_folder(&roots, &path, &[]);
    assert_eq!(covered(recordings.join("x.mp4")), Some(None));
    assert_eq!(covered(recordings.join("exports").join("x.mp4")), None, "the recordings folder lists flat");
    assert_eq!(covered(imported.join("a").join("x.mp4")), Some(Some("a".into())));
    assert_eq!(covered(imported.join("a").join("b").join("c").join("x.mp4")), Some(Some("c".into())), "as deep as the walk goes");
    assert_eq!(covered(imported.join("a").join("b").join("c").join("d").join("x.mp4")), None, "deeper than the walk");
    assert_eq!(covered(imported.join("thumbs").join("x.mp4")), None);
    assert_eq!(covered(dir.join("elsewhere.mp4")), None);
    let cache = imported.join("a");
    assert_eq!(covering_folder(&roots, &cache.join("x.mp4"), std::slice::from_ref(&cache)), None, "Recall's own cache");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_settle_step_takes_ownership_from_the_listing_not_from_disk() {
    let watcher = ClipWatcher::start(|_| {});
    assert!(watcher.owned().is_empty());
    watcher.set_owned(vec![r"E:\Old Recall".into()]);
    assert_eq!(watcher.owned(), [r"E:\Old Recall".to_owned()]);
    watcher.stop();
}

#[test]
fn a_cached_walk_learns_changes_without_a_rewalk() {
    let dir = temp("patch");
    fs::create_dir_all(dir.join("g")).unwrap();
    fs::write(dir.join("g").join("old.mp4"), b"x").unwrap();
    fs::write(dir.join("keep.mp4"), b"x").unwrap();
    assert_eq!(folders::scan_imported_cached(&dir, &[]).len(), 2);
    // These reach the listing only through the patch: the cached walk is not refreshed.
    let new = folders::FoundClip { path: dir.join("new.mp4"), bytes: 9, mtime: 1, folder: None };
    folders::patch_scans(std::slice::from_ref(&new), &[], &[dir.join("g").to_string_lossy().into_owned()]);
    let names: Vec<_> = folders::scan_imported_cached(&dir, &[]).iter().map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(names.contains(&"new.mp4".to_owned()) && names.contains(&"keep.mp4".to_owned()) && names.len() == 2, "{names:?}");
    folders::forget_scan(&dir);
    assert_eq!(folders::scan_imported_cached(&dir, &[]).len(), 2, "really on disk: keep and old");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_probe_sees_writers_folders_and_missing_files() {
    let dir = temp("probe");
    let path = dir.join("a.mp4");
    let writer = File::create(&path).unwrap();
    assert_eq!(probe(&path, Kind::Clip), Probe::Busy);
    drop(writer);
    assert!(matches!(probe(&path, Kind::Clip), Probe::Ready { bytes: 0, .. }));
    let reader = File::open(&path).unwrap();
    assert!(matches!(probe(&path, Kind::Clip), Probe::Ready { .. }), "a player reading it does not block");
    drop(reader);
    assert_eq!(probe(&dir, Kind::Other), Probe::Directory);
    assert_eq!(probe(&dir.join("gone.mp4"), Kind::Clip), Probe::Missing);
    let _ = fs::remove_dir_all(&dir);
}

// ---- real folders ------------------------------------------------------------------------

/// Deleted when the test ends, panics included (`TempDir`).
fn temp(name: &str) -> crate::test_temp::TempDir {
    crate::test_temp::TempDir::new(format!("recall-watch-{name}-{}", std::process::id()))
}

fn watcher(roots: &[(&Path, bool)]) -> (ClipWatcher, Receiver<LibraryChange>) {
    let (tx, rx) = mpsc::channel();
    let watcher = ClipWatcher::start(move |change| {
        let _ = tx.send(change);
    });
    watcher.set_roots(roots.iter().map(|(path, recursive)| WatchRoot { path: path.to_path_buf(), recursive: *recursive }).collect());
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(LibraryChange::RootsChanged));
    (watcher, rx)
}

/// Clip changes until `done` holds for what arrived, or `limit` passes.
fn gather(rx: &Receiver<LibraryChange>, limit: Duration, mut done: impl FnMut(&ClipChanges) -> bool) -> (ClipChanges, Vec<LibraryChange>) {
    let deadline = Instant::now() + limit;
    let (mut all, mut others) = (ClipChanges::default(), Vec::new());
    while !done(&all) {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(LibraryChange::Clips(changes)) => {
                all.added.extend(changes.added);
                all.gone.extend(changes.gone);
                all.gone_under.extend(changes.gone_under);
            }
            Ok(other) => others.push(other),
            Err(_) => break,
        }
    }
    (all, others)
}

fn names(changes: &ClipChanges) -> Vec<&str> {
    changes.added.iter().map(|a| a.entry.name.as_str()).collect()
}

const SETTLED: Duration = Duration::from_secs(2);

#[test]
fn a_real_copy_shows_only_after_the_writer_closes() {
    let dir = temp("copy");
    let (watcher, rx) = watcher(&[(&dir, false)]);
    let path = dir.join("slow.mp4");
    let mut writer = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    // CopyFile's shape: full size first, the data after.
    writer.set_len(1 << 20).unwrap();
    for _ in 0..6 {
        writer.write_all(&[7; 4096]).unwrap();
        writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    }
    let (held, _) = gather(&rx, Duration::from_millis(1200), |c| !c.added.is_empty());
    assert!(held.added.is_empty(), "a tile for a file still open: {held:?}");
    drop(writer);
    let closed = Instant::now();
    let (changes, _) = gather(&rx, SETTLED, |c| !c.added.is_empty());
    let took = closed.elapsed();
    assert_eq!(names(&changes), ["slow.mp4"]);
    assert_eq!(changes.added[0].entry.bytes, 1 << 20);
    assert_eq!(changes.added[0].entry.path, path.to_string_lossy());
    assert!(took < Duration::from_secs(2), "{took:?}");
    println!("tile {} ms after close", took.as_millis());
    watcher.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn part_files_stay_out_and_the_rename_makes_one_clip() {
    let dir = temp("part");
    let (watcher, rx) = watcher(&[(&dir, false)]);
    fs::write(dir.join("a.mp4.part"), [1u8; 2048]).unwrap();
    fs::write(dir.join("notes.txt"), b"x").unwrap();
    let (quiet, _) = gather(&rx, Duration::from_millis(1200), |c| !c.added.is_empty());
    assert!(quiet.added.is_empty());
    fs::rename(dir.join("a.mp4.part"), dir.join("a.mp4")).unwrap();
    let (changes, _) = gather(&rx, SETTLED, |c| !c.added.is_empty());
    assert_eq!(names(&changes), ["a.mp4"]);
    watcher.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renames_and_deletes_reach_the_library() {
    let dir = temp("rename");
    fs::write(dir.join("a.mp4"), [1u8; 100]).unwrap();
    fs::write(dir.join("b.mp4"), [1u8; 100]).unwrap();
    let (watcher, rx) = watcher(&[(&dir, false)]);
    fs::rename(dir.join("a.mp4"), dir.join("c.mp4")).unwrap();
    fs::remove_file(dir.join("b.mp4")).unwrap();
    let (changes, _) = gather(&rx, SETTLED, |c| c.gone.len() == 2 && !c.added.is_empty());
    assert_eq!(names(&changes), ["c.mp4"]);
    let mut gone: Vec<_> = changes.gone.iter().map(|p| Path::new(p).file_name().unwrap().to_string_lossy().into_owned()).collect();
    gone.sort();
    assert_eq!(gone, ["a.mp4", "b.mp4"]);
    watcher.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn imported_folders_see_subfolders_arriving_and_leaving() {
    let dir = temp("nested");
    let root = dir.join("imported");
    let outside = dir.join("outside");
    fs::create_dir_all(root.join("Rocket League")).unwrap();
    fs::create_dir_all(outside.join("Old Game").join("deep")).unwrap();
    fs::write(outside.join("Old Game").join("moved.mp4"), [1u8; 100]).unwrap();
    fs::write(outside.join("Old Game").join("deep").join("moved2.mp4"), [1u8; 100]).unwrap();
    let (watcher, rx) = watcher(&[(&root, true)]);

    fs::write(root.join("Rocket League").join("goal.mp4"), [1u8; 100]).unwrap();
    let (changes, _) = gather(&rx, SETTLED, |c| !c.added.is_empty());
    assert_eq!(changes.added[0].entry.folder.as_deref(), Some("Rocket League"));

    // A move on one volume sends one event, for the folder.
    fs::rename(outside.join("Old Game"), root.join("Old Game")).unwrap();
    let (moved, _) = gather(&rx, SETTLED, |c| c.added.len() >= 2);
    let mut found = names(&moved);
    found.sort();
    assert_eq!(found, ["moved.mp4", "moved2.mp4"]);

    fs::rename(root.join("Old Game"), outside.join("Old Game")).unwrap();
    let (left, _) = gather(&rx, SETTLED, |c| !c.gone_under.is_empty());
    assert!(left.gone_under[0].ends_with("Old Game"), "{left:?}");
    watcher.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn folders_added_and_removed_while_watching_take_effect() {
    let (first, second) = (temp("roots-a"), temp("roots-b"));
    let (watcher, rx) = watcher(&[(&first, false)]);
    let both = vec![WatchRoot { path: first.clone(), recursive: false }, WatchRoot { path: second.clone(), recursive: true }];
    watcher.set_roots(both.clone());
    fs::write(second.join("late.mp4"), [1u8; 100]).unwrap();
    let (changes, others) = gather(&rx, SETTLED, |c| !c.added.is_empty());
    assert_eq!(names(&changes), ["late.mp4"]);
    assert_eq!(others, [LibraryChange::RootsChanged]);

    watcher.set_roots(both);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "same folders: no restart, no noise");

    // A clip pending in a folder removed meanwhile never shows.
    fs::write(second.join("orphan.mp4"), [1u8; 100]).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    watcher.set_roots(vec![WatchRoot { path: first.clone(), recursive: false }]);
    fs::write(second.join("after.mp4"), [1u8; 100]).unwrap();
    let (nothing, _) = gather(&rx, Duration::from_millis(1500), |c| !c.added.is_empty());
    assert!(nothing.added.is_empty(), "{nothing:?}");
    watcher.stop();
    let _ = (fs::remove_dir_all(&first), fs::remove_dir_all(&second));
}

/// Waits for a list of non-clip changes, ignoring clip batches.
fn wait_for(rx: &Receiver<LibraryChange>, limit: Duration, wanted: &[LibraryChange]) -> Vec<LibraryChange> {
    let deadline = Instant::now() + limit;
    let mut seen = Vec::new();
    while !wanted.iter().all(|w| seen.contains(w)) {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(LibraryChange::Clips(_)) => {}
            Ok(other) => seen.push(other),
            Err(_) => break,
        }
    }
    seen
}

#[test]
fn a_folder_that_goes_away_and_comes_back_is_watched_again() {
    let parent = temp("lost");
    let root = parent.join("clips");
    let away = parent.join("away");
    fs::create_dir_all(&root).unwrap();
    let text = root.to_string_lossy().into_owned();
    let (watcher, rx) = watcher(&[(&root, false)]);

    // A rename keeps the OS handle working and sends no event of its own.
    fs::rename(&root, &away).unwrap();
    let lost = [LibraryChange::FolderLost(text.clone()), LibraryChange::Resync];
    let seen = wait_for(&rx, Duration::from_secs(8), &lost);
    assert!(lost.iter().all(|w| seen.contains(w)), "{seen:?}");

    fs::rename(&away, &root).unwrap();
    let back = [LibraryChange::FolderBack(text.clone())];
    let seen = wait_for(&rx, Duration::from_secs(5), &back);
    assert!(seen.contains(&back[0]), "{seen:?}");

    fs::write(root.join("after.mp4"), [1u8; 100]).unwrap();
    let (changes, _) = gather(&rx, SETTLED, |c| !c.added.is_empty());
    assert_eq!(names(&changes), ["after.mp4"]);

    // Deleted outright, then made again.
    fs::remove_dir_all(&root).unwrap();
    let seen = wait_for(&rx, Duration::from_secs(8), &[LibraryChange::FolderLost(text.clone())]);
    assert!(seen.contains(&LibraryChange::FolderLost(text.clone())), "{seen:?}");
    fs::create_dir_all(&root).unwrap();
    let seen = wait_for(&rx, Duration::from_secs(5), &back);
    assert!(seen.contains(&back[0]), "{seen:?}");
    watcher.stop();
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn a_folder_missing_at_start_is_picked_up_when_it_appears() {
    let parent = temp("missing");
    let root = parent.join("later");
    let (watcher, rx) = watcher(&[(&root, true)]);
    fs::create_dir_all(&root).unwrap();
    let back = LibraryChange::FolderBack(root.to_string_lossy().into_owned());
    let seen = wait_for(&rx, Duration::from_secs(5), std::slice::from_ref(&back));
    assert!(seen.contains(&back), "{seen:?}");
    assert!(!seen.iter().any(|c| matches!(c, LibraryChange::FolderLost(_))), "nothing was lost");
    watcher.stop();
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn a_burst_of_real_files_arrives_as_few_updates() {
    let dir = temp("burst");
    let (watcher, rx) = watcher(&[(&dir, false)]);
    let total = 400;
    for index in 0..total {
        fs::write(dir.join(format!("{index:04}.mp4")), [1u8; 512]).unwrap();
    }
    let wrote = Instant::now();
    let (mut count, mut updates, deadline) = (0, 0, wrote + Duration::from_secs(5));
    while count < total {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(LibraryChange::Clips(changes)) => {
                count += changes.added.len();
                updates += 1;
            }
            Ok(_) => {}
            Err(_) => panic!("only {count} of {total}"),
        }
    }
    println!("{total} files: {updates} updates in {} ms", wrote.elapsed().as_millis());
    assert!(updates <= 8, "{updates} updates");
    watcher.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stopping_ends_every_thread_and_later_calls_do_nothing() {
    let dir = temp("stop");
    let (watcher, rx) = watcher(&[(&dir, true)]);
    let started = Instant::now();
    watcher.stop();
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    watcher.stop();
    watcher.set_roots(vec![WatchRoot { path: dir.clone(), recursive: false }]);
    fs::write(dir.join("late.mp4"), [1u8; 100]).unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(1)).is_err(), "the sink went with the settle thread");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_linked_clip_is_probed_without_shutting_out_its_writer() {
    // Another recorder (SteelSeries, ShadowPlay) writes its clip sharing read and write: a probe
    // that shares only read and delete fails against it, and while open it would refuse that
    // recorder a write open. Linked roots are probed with share mode 7 (read, write, delete).
    let dir = std::env::temp_dir().join(format!("recall-watch-linked-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("moment.mp4");
    fs::write(&path, b"moment").unwrap();
    use std::os::windows::fs::OpenOptionsExt;
    let writer = OpenOptions::new().write(true).share_mode(1 | 2).open(&path).unwrap();
    assert!(matches!(probe_in(&path, Kind::Clip, true), Probe::Ready { bytes: 6, .. }), "the linked probe shares write");
    // And its open leaves the other recorder free to open the clip for write again.
    assert_eq!(super::watch::probe_share(true), 7);
    drop(writer);
    let _ = fs::remove_dir_all(&dir);
}

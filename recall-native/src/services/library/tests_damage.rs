//! library.json / folders.json damage across restarts (deep-audit round 2: R2-1, R2-2,
//! L6/S12, R2-11). Only inside fresh temp folders.
use std::path::{Path, PathBuf};

use super::folders::Folders;
use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-native-damage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A library.json cut off by a power cut: two favourites, the second one mid-entry.
const TRUNCATED: &[u8] = br#"{"a.mp4":{"favourite":true,"title":"Ace"},"b.mp4":{"favourite":true,"dura"#;

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    out.sort();
    out
}

#[test]
fn the_hold_survives_a_restart_and_the_favourites_are_recovered() {
    let dir = temp_dir("restart");
    let path = dir.join("library.json");
    std::fs::write(&path, TRUNCATED).unwrap();
    drop(MetaStore::load_from(path.clone()));
    // Run 2: durations arrive and the debounced flush runs.
    let mut store = MetaStore::load_from(path.clone());
    store.set_duration("c.mp4", 10, 20, 30.0);
    store.flush().unwrap();
    assert!(!path.exists(), "a restart does not end the hold: {:?}", names(&dir));
    assert!(store.meta("a.mp4").is_some_and(|m| m.favourite && m.title.as_deref() == Some("Ace")), "salvaged from the .bad");
    assert!(store.meta("b.mp4").is_some_and(|m| m.favourite), "an entry cut off after its star keeps it");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_star_edit_alone_does_not_end_the_hold() {
    let dir = temp_dir("edit");
    let path = dir.join("library.json");
    std::fs::write(&path, TRUNCATED).unwrap();
    let mut store = MetaStore::load_from(path.clone());
    store.set_favourite("c.mp4", true).unwrap();
    drop(store);
    let mut back = MetaStore::load_from(path.clone());
    assert!(back.meta("c.mp4").is_some_and(|m| m.favourite), "the star itself is saved");
    let before = std::fs::read(&path).unwrap();
    back.set_duration("d.mp4", 1, 2, 3.0);
    back.flush().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before, "still held after the edit and a restart");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn keep_going_writes_the_recovered_favourites_and_ends_the_hold() {
    let dir = temp_dir("keep-going");
    let path = dir.join("library.json");
    std::fs::write(&path, TRUNCATED).unwrap();
    let mut store = MetaStore::load_from(path.clone());
    let damage = store.damage().cloned().expect("the notice has something to say");
    assert_eq!(damage.recovered, 2);
    assert!(damage.backup.is_some_and(|b| b.exists()));
    store.keep_going().unwrap();
    assert!(store.damage().is_none());
    assert!(!dir.join("library.json.hold").exists());
    let mut back = MetaStore::load_from(path.clone());
    assert!(back.damage().is_none(), "confirmed: no notice on the next start");
    assert!(back.meta("a.mp4").is_some_and(|m| m.favourite), "written back");
    back.set_duration("d.mp4", 1, 2, 3.0);
    back.flush().unwrap();
    assert_eq!(MetaStore::load_from(path).duration("d.mp4", "d.mp4", 1, 2), Some(3.0), "background writes again");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_background_flush_lands_in_order_off_the_caller() {
    let dir = temp_dir("async");
    let path = dir.join("library.json");
    let mut store = MetaStore::load_from(path.clone());
    for secs in 1..=5 {
        store.set_duration("a.mp4", 1, 2, f64::from(secs));
        store.flush_in_background().unwrap();
    }
    store.set_favourite("b.mp4", true).unwrap();
    store.wait_for_writes();
    let back = MetaStore::load_from(path);
    assert_eq!(back.duration("a.mp4", "a.mp4", 1, 2), Some(5.0), "the newest flush is on disk");
    assert!(back.meta("b.mp4").is_some_and(|m| m.favourite));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_damaged_folders_file_keeps_the_imported_folders_it_still_holds() {
    let dir = temp_dir("folders");
    let path = dir.join("folders.json");
    std::fs::write(&path, br#"{"recordings":"E:\\Clips","imported":["D:\\Old","F:\\Older","G:\\Cu"#).unwrap();
    let folders = Folders::load_from(&path);
    assert_eq!(folders.imported, vec![r"D:\Old".to_owned(), r"F:\Older".to_owned()]);
    assert_eq!(folders.recordings.as_deref(), Some(r"E:\Clips"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn leftover_temp_files_are_swept_when_the_library_loads() {
    let dir = temp_dir("temps");
    let path = dir.join("library.json");
    std::fs::write(&path, b"{}").unwrap();
    let tmp = dir.join("library.json.4242.tmp");
    std::fs::write(&tmp, b"partial").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
    std::fs::OpenOptions::new().write(true).open(&tmp).unwrap().set_modified(old).unwrap();
    drop(MetaStore::load_from(path));
    assert_eq!(names(&dir), vec!["library.json".to_owned()]);
    let _ = std::fs::remove_dir_all(dir);
}

//! Corrupt or locked library files (deep-audit L1, L6, L7, S12): the user's data is never
//! overwritten by a fresh empty file. Only inside fresh temp folders.
use std::path::{Path, PathBuf};

use super::folders::Folders;
use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-native-persist-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `<file name>.bad*` copies beside `path`, with their bytes.
fn backups(path: &Path) -> Vec<Vec<u8>> {
    let prefix = format!("{}.bad", path.file_name().unwrap().to_string_lossy());
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| std::fs::read(e.path()).unwrap())
        .collect()
}

/// Opens `path` with no sharing (as an antivirus scan or a sync client can).
fn lock(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).share_mode(0).open(path).unwrap()
}

#[test]
fn a_fresh_store_after_a_bad_file_is_not_written_until_a_user_edit() {
    let dir = temp_dir("fresh");
    let path = dir.join("library.json");
    let truncated = br#"{"a.mp4":{"favourite":true}"#;
    std::fs::write(&path, truncated).unwrap();
    let mut store = MetaStore::load_from(path.clone());
    assert_eq!(backups(&path), vec![truncated.to_vec()], "the damaged bytes are kept");
    store.set_duration("b.mp4", 10, 20, 30.0);
    store.flush().unwrap();
    store.set_played("b.mp4", 1_700_000_000).unwrap();
    assert!(!path.exists(), "background writes (durations, played) do not create a fresh library.json");
    store.set_favourite("c.mp4", true).unwrap();
    let back = MetaStore::load_from(path.clone());
    assert!(back.meta("c.mp4").unwrap().favourite, "a user edit writes");
    assert_eq!(back.duration("b.mp4", 10, 20), Some(30.0), "with the held background entries");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_corrupt_folders_file_is_kept_as_bad_and_a_save_does_not_touch_it() {
    let dir = temp_dir("folders-bad");
    let path = dir.join("folders.json");
    let truncated = br#"{"imported":["D:\\Old"]"#;
    std::fs::write(&path, truncated).unwrap();
    let mut folders = Folders::load_from(&path);
    folders.recordings = Some(r"E:\Clips".into());
    let _ = folders.save_to(&path);
    assert_eq!(backups(&path), vec![truncated.to_vec()], "the original bytes survive as .bad");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_unreadable_folders_file_is_never_overwritten() {
    let dir = temp_dir("folders-locked");
    let path = dir.join("folders.json");
    let good = br#"{"recordings":"E:\\Clips","imported":["D:\\Old"]}"#;
    std::fs::write(&path, good).unwrap();
    let held = lock(&path);
    let mut folders = Folders::load_from(&path);
    drop(held);
    folders.recordings = Some(r"F:\New".into());
    assert!(folders.save_to(&path).is_err(), "a save after a failed read is refused");
    assert_eq!(std::fs::read(&path).unwrap(), good.to_vec());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_corrupt_imports_table_survives_as_bad_after_a_save() {
    let dir = temp_dir("imports-bad");
    let path = dir.join("imports.json");
    let truncated = br#"{"version":1,"files":{"d:\\old\\a.mp4":{"path":"D:\\Old\\a.mp4","hidden":true}"#;
    std::fs::write(&path, truncated).unwrap();
    let table = imports::load_table(&path);
    let _ = imports::save_table(&path, &table);
    assert_eq!(backups(&path), vec![truncated.to_vec()], "the original bytes survive as .bad");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_imports_table_keeps_good_entries_beside_one_from_a_newer_build() {
    let dir = temp_dir("imports-entry");
    let path = dir.join("imports.json");
    let text = br#"{"version":1,"files":{"d:\\old\\a.mp4":{"path":"D:\\Old\\a.mp4","hidden":true},"d:\\old\\b.mp4":{"path":"D:\\Old\\b.mp4","kind":"future_kind"}}}"#;
    std::fs::write(&path, text).unwrap();
    let table = imports::load_table(&path);
    assert!(imports::table_hides(&table, r"d:\old\a.mp4"), "the removed clip stays removed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_unreadable_imports_table_is_never_overwritten() {
    let dir = temp_dir("imports-locked");
    let path = dir.join("imports.json");
    let good = br#"{"version":1,"files":{}}"#;
    std::fs::write(&path, good).unwrap();
    let held = lock(&path);
    let table = imports::load_table(&path);
    drop(held);
    assert!(imports::save_table(&path, &table).is_err(), "a save after a failed read is refused");
    assert_eq!(std::fs::read(&path).unwrap(), good.to_vec());
    let _ = std::fs::remove_dir_all(dir);
}

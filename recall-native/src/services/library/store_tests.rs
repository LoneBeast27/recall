//! `MetaStore` key migration.
use super::super::{MetaStore, clip_key};

#[test]
fn a_name_entry_merges_into_an_existing_path_entry_field_by_field() {
    // An older build starred the clip by name; this build already cached its length by path.
    let mut store = MetaStore::default();
    store.set_favourite("x.mp4", true).unwrap();
    store.set_title("x.mp4", Some("Ace")).unwrap();
    let path = r"D:\Clips\x.mp4";
    store.set_duration(&clip_key(path), 10, 20, 31.5);
    store.set_played(&clip_key(path), 900).unwrap();
    assert!(store.migrate_names([(path, "x.mp4")]));
    let meta = store.meta(&clip_key(path)).expect("path entry");
    assert!(meta.favourite, "the star set under the old build is kept");
    assert_eq!(meta.title.as_deref(), Some("Ace"));
    assert_eq!(meta.played, Some(900));
    assert_eq!(meta.duration.map(|d| d.secs), Some(31.5));
    assert!(store.meta("x.mp4").is_none());
}

#[test]
fn an_unreadable_length_is_remembered_until_the_file_changes() {
    let dir = std::env::temp_dir().join(format!("recall-store-nodur-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("library.json");
    let path = r"D:\Clipsroken.mkv";
    let mut store = MetaStore::load_from(file.clone());
    store.set_duration_unknown(&clip_key(path), 100, 7);
    store.flush().unwrap();
    let again = MetaStore::load_from(file.clone());
    assert!(again.duration_unknown(path, "broken.mkv", 100, 7), "the next start does not probe it again");
    assert!(!again.duration_unknown(path, "broken.mkv", 101, 7), "a changed file is probed again");
    let mut fixed = again;
    fixed.set_duration(&clip_key(path), 100, 7, 12.0);
    assert!(!fixed.duration_unknown(path, "broken.mkv", 100, 7));
    let _ = std::fs::remove_dir_all(&dir);
}

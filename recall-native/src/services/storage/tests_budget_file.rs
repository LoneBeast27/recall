//! storage.json and the cache budget: only a missing file gets the first-run pick written; a
//! locked or damaged one keeps its bytes (the user's 20 GiB on C: must survive a passing lock).
use std::path::{Path, PathBuf};

const USER: &[u8] = br#"{ "cache_budget_bytes": 21474836480 }"#;

fn root(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-budget-file-{tag}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn bads(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("storage.json.bad")).collect()
}

#[test]
fn a_locked_storage_file_is_never_written_over() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = root("locked");
    let file = root.join("storage.json");
    std::fs::write(&file, USER).unwrap();
    // Another process holds it: no one else may read it (FILE_SHARE_DELETE only, so a replace
    // over it would still go through).
    let held = std::fs::OpenOptions::new().read(true).share_mode(4).open(&file).unwrap();
    super::video::budget_in(&root);
    drop(held);
    assert_eq!(std::fs::read(&file).unwrap(), USER.to_vec(), "byte for byte");
    assert!(bads(&root).is_empty(), "a lock is not damage: no .bad copy");
    assert_eq!(super::video::budget_in(&root), 20 << 30, "read again once the lock passed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_corrupt_storage_file_is_copied_to_bad_and_left_in_place() {
    let root = root("corrupt");
    let file = root.join("storage.json");
    let half = br#"{ "cache_budget_bytes": 2147"#;
    std::fs::write(&file, half).unwrap();
    super::video::budget_in(&root);
    super::video::budget_in(&root);
    assert_eq!(std::fs::read(&file).unwrap(), half.to_vec(), "the file keeps its bytes");
    let kept = bads(&root);
    assert_eq!(kept.len(), 1, "one backup however often it is read: {kept:?}");
    assert_eq!(std::fs::read(&kept[0]).unwrap(), half.to_vec());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_a_missing_storage_file_gets_the_first_run_pick_written() {
    let root = root("absent");
    let picked = super::video::budget_in(&root);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("storage.json")).unwrap()).unwrap();
    assert_eq!(saved["cache_budget_bytes"].as_u64(), Some(picked));
    assert_eq!(saved["first_run"].as_bool(), Some(true));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_20_gib_budget_on_the_system_drive_stays_20_gib() {
    // Rohan's real storage.json (cache on C:, budget kept at 20 GiB): the C2 2 GiB first-run
    // rule never touches it.
    let root = root("twenty");
    assert!(super::drive::on_system_drive(&root), "the test cache sits on the system drive");
    std::fs::write(root.join("storage.json"), USER).unwrap();
    assert_eq!(super::video::budget_in(&root), 21_474_836_480);
    assert_eq!(super::video::budget_in(&root), 21_474_836_480, "and again");
    assert_eq!(std::fs::read(root.join("storage.json")).unwrap(), USER.to_vec(), "unchanged on disk");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_locked_storage_file_keeps_the_budget_read_earlier_and_refuses_a_save() {
    // RI2-2: a passing lock gave the 2 GiB pick, which evicted the 20 GiB cache and could be
    // saved by a stray click in Settings.
    use std::os::windows::fs::OpenOptionsExt;
    let root = root("locked-later");
    let file = root.join("storage.json");
    std::fs::write(&file, USER).unwrap();
    assert_eq!(super::video::budget_in(&root), 20 << 30);
    let held = std::fs::OpenOptions::new().read(true).share_mode(4).open(&file).unwrap();
    assert_eq!(super::video::budget_in(&root), 20 << 30, "the budget read earlier, not the pick");
    assert!(super::video::budget_note(&root).is_some(), "Settings shows the size read-only");
    assert!(super::save_budget_in(&root, 2 << 30).is_err(), "a save is refused while it can't be read");
    drop(held);
    assert_eq!(std::fs::read(&file).unwrap(), USER.to_vec(), "byte for byte");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn eviction_never_goes_below_the_cache_on_disk_while_the_budget_is_unknown() {
    let base = root("evict-unknown");
    let (cache, clips) = (base.join("cache"), base.join("clips"));
    std::fs::create_dir_all(&clips).unwrap();
    let clip = clips.join("G__2026-11-07__10-00-00-000__aa.mp4");
    std::fs::write(&clip, vec![9u8; 3 << 20]).unwrap();
    let copy = super::video::cache_in(&cache, &clip, 20 << 20).unwrap();
    std::fs::write(cache.join("storage.json"), br#"{ "cache_budget_bytes": 2147"#).unwrap();
    super::budget::enforce(&cache, 0, 0);
    assert!(copy.exists(), "a damaged storage.json evicts nothing already cached");
    std::fs::remove_file(cache.join("storage.json")).unwrap();
    super::budget::enforce(&cache, 0, 0);
    assert!(!copy.exists(), "a known budget evicts as before");
    let _ = std::fs::remove_dir_all(&base);
}

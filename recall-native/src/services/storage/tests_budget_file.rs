//! storage.json and the cache budget: only a missing file gets the first-run pick written; a
//! locked or damaged one keeps its bytes (the user's 20 GiB on C: must survive a passing lock).
use std::path::{Path, PathBuf};

const USER: &[u8] = br#"{ "cache_budget_bytes": 21474836480 }"#;

/// `%TEMP%\recall-budget-file-<tag>-<pid>-<nonce>`, removed when the guard drops, also when the
/// test panics (a failing run left 2-3 MB folders behind).
fn root(tag: &str) -> crate::test_temp::TempDir {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    crate::test_temp::TempDir::new(format!("recall-budget-file-{tag}-{}-{nonce}", std::process::id()))
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

#[test]
fn an_unknown_cache_size_copies_no_clip_on_open() {
    // RI3-1: copy-on-open kept growing the C: cache while storage.json was damaged.
    let base = root("open-unknown");
    let (cache, clips) = (base.join("cache"), base.join("clips"));
    std::fs::create_dir_all(&clips).unwrap();
    let clip = clips.join("G__2026-11-07__12-00-00-000__aa.mp4");
    std::fs::write(&clip, vec![9u8; 1 << 20]).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("storage.json"), b"\0\0\0").unwrap();
    assert!(super::video::cache_in(&cache, &clip, 20 << 20).unwrap_err().contains("unknown"));
    assert!(super::cached_video_in(&cache, &clip).is_none(), "the clip plays from its own drive");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_damaged_storage_file_can_be_reset_and_a_locked_one_cannot() {
    // RI3-1: a damaged storage.json left the size read-only for good, with no way out in the app.
    use std::os::windows::fs::OpenOptionsExt;
    let root = root("reset");
    let file = root.join("storage.json");
    std::fs::write(&file, USER).unwrap();
    assert_eq!(super::video::budget_in(&root), 20 << 30, "read once this session");
    let held = std::fs::OpenOptions::new().read(true).share_mode(4).open(&file).unwrap();
    assert!(!super::video::budget_resettable(&root), "a lock is not damage");
    assert!(super::video::reset_budget(&root).is_err());
    drop(held);
    std::fs::write(&file, b"\0\0\0\0").unwrap();
    assert!(super::video::budget_resettable(&root), "damaged, and the .bad copy exists");
    assert!(!bads(&root).is_empty());
    assert_eq!(super::video::reset_budget(&root), Ok(20 << 30), "the budget read earlier");
    assert!(super::video::budget_note(&root).is_none(), "known again");
    assert_eq!(super::video::budget_in(&root), 20 << 30);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved["cache_budget_bytes"].as_u64(), Some(20 << 30));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_fixture_whose_test_panics_still_removes_its_folder() {
    let left = std::panic::catch_unwind(|| {
        let dir = root("panics");
        std::fs::write(dir.join("storage.json"), vec![1u8; 4096]).unwrap();
        std::panic::panic_any(dir.to_path_buf());
    })
    .unwrap_err()
    .downcast::<PathBuf>()
    .unwrap();
    assert!(!left.exists(), "{} was left behind", left.display());
}

#[test]
fn leftover_copies_do_not_hold_the_cache_budget_but_show_in_the_usage() {
    // RI3-4: kept\leaked\ counted as fixed bytes, so a few leftover copies made the budget pass
    // empty every evictable tier.
    let base = root("leaked-budget");
    let (cache, clips) = (base.join("cache"), base.join("clips"));
    std::fs::create_dir_all(&clips).unwrap();
    let clip = clips.join("G__2026-11-07__13-00-00-000__aa.mp4");
    std::fs::write(&clip, vec![9u8; 3 << 20]).unwrap();
    let copy = super::video::cache_in(&cache, &clip, 20 << 20).unwrap();
    let leaked = cache.join("staging").join("kept").join("leaked");
    std::fs::create_dir_all(&leaked).unwrap();
    std::fs::write(leaked.join("L__2026-11-07__13-01-00-000__ll.mp4"), vec![1u8; 6 << 20]).unwrap();
    super::budget::enforce(&cache, 8 << 20, 0);
    assert!(copy.exists(), "3 MiB of cache fits an 8 MiB budget; the 6 MiB of leftovers don't count");
    let usage = super::ssd_usage::ssd_usage_in(&cache, &clips);
    assert!(usage.kept >= 9 << 20, "the usage shown still holds the leftovers: {}", usage.kept);
    let _ = std::fs::remove_dir_all(&base);
}

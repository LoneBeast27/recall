//! Cache safety, second pass: the dry run writes nothing, a corrupt or locked `owned.json` adopts
//! nothing, old unmarked kept copies go only within a cap, visuals folders do not leak, a blocked
//! cache is re-checked, and one folder reached through another name still counts as the same.
//! Sandbox folders under the worktree's `.tmp` only.
use std::collections::BTreeMap;
use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;
use crate::test_temp::TempDir;

const A: &str = "aaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccc";

fn write(path: &Path, len: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![9u8; len]).unwrap();
}

fn aged(path: &Path, days: u64) {
    let at = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(at).set_created(at)).unwrap();
}

/// Every file and folder under `dir` with its full bytes (folders: `None`).
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut all = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                all.insert(path.clone(), None);
                stack.push(path);
            } else {
                all.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    all
}

fn cache(tag: &str) -> (TempDir, PathBuf) {
    let sandbox = TempDir::sandbox(format!("recall-safety-{tag}"));
    let root = sandbox.join("cache");
    std::fs::create_dir_all(&root).unwrap();
    (sandbox, root)
}

#[test]
fn a_dry_run_writes_nothing_and_says_what_it_would_adopt_and_evict() {
    let (_sandbox, root) = cache("dry");
    write(&root.join("video").join(format!("{A}.mp4")), 4096);
    write(&root.join("light").join(format!("{B}.mp4")), 4096);
    write(&root.join("thumbs").join(format!("{A}.jpg")), 300);
    write(&root.join("editor").join(C).join("clip.json"), 200);
    write(&root.join("editor").join(C).join("audio-0.flac"), 4096);
    write(&root.join("video").join("My clip.mp4"), 4096);
    let before = snapshot(&root);
    let (adopt, _) = plan_dry(&root);
    assert_eq!(snapshot(&root), before, "byte-identical: no owned.json, no first-run storage.json");
    for own in [format!("video/{A}.mp4"), format!("light/{B}.mp4")] {
        assert!(adopt.contains(&own), "{own} would be adopted: {adopt:?}");
    }
    assert!(!adopt.iter().any(|name| name.contains("My clip")), "a user clip is never adopted");
    std::fs::write(root.join("storage.json"), br#"{ "cache_budget_bytes": 1 }"#).unwrap();
    let before = snapshot(&root);
    let (_, evict) = plan_dry(&root);
    assert_eq!(snapshot(&root), before, "still byte-identical");
    for unit in [format!("video/{A}.mp4"), format!("light/{B}.mp4"), format!("editor/{C}")] {
        assert!(evict.contains(&unit), "{unit} would be evicted: {evict:?}");
    }
    assert!(!evict.iter().any(|name| name.contains("My clip")));
}

fn corrupt_siblings(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("owned.json.corrupt-")).map(|e| e.path()).collect()
}

#[test]
fn a_corrupt_ownership_list_is_set_aside_and_nothing_is_adopted() {
    let (_sandbox, root) = cache("corrupt");
    let copy = root.join("video").join(format!("{A}.mp4"));
    write(&copy, 4096);
    let broken = br#"{"files": ["video/aaaa"#;
    std::fs::write(root.join("owned.json"), broken).unwrap();
    budget::enforce(&root, 0, 0);
    assert!(copy.exists(), "a corrupt list adopts nothing, so nothing is evicted");
    let aside = corrupt_siblings(&root);
    assert_eq!(aside.len(), 1, "set aside, never deleted: {aside:?}");
    assert_eq!(std::fs::read(&aside[0]).unwrap(), broken, "the bytes are kept as they were");
    budget::enforce(&root, 0, 0);
    assert!(copy.exists(), "the next pass does not adopt either");
}

#[test]
fn a_locked_ownership_list_is_left_alone_and_nothing_is_adopted() {
    let (_sandbox, root) = cache("locked");
    let copy = root.join("video").join(format!("{A}.mp4"));
    write(&copy, 4096);
    let list = root.join("owned.json");
    let text = format!(r#"{{"files":["video/{A}.mp4"]}}"#);
    std::fs::write(&list, &text).unwrap();
    let handle = std::fs::File::options().read(true).share_mode(0).open(&list).unwrap();
    budget::enforce(&root, 0, 0);
    assert!(copy.exists(), "an unreadable list adopts nothing, so nothing is evicted");
    drop(handle);
    assert_eq!(std::fs::read_to_string(&list).unwrap(), text, "the list is not overwritten");
}

/// Review 11-07: unmarked kept copies from before the mark may be the only copy of a clip;
/// the start-up sweep leaves every one of them (a leak, never a delete).
#[test]
fn old_unmarked_kept_copies_are_never_deleted() {
    let (sandbox, root) = cache("kept-cap");
    let kept = sandbox.join("kept");
    let old = kept.join("VALORANT__2026-10-01__10-00-00-000__0123456789abcdef0123456789abcdef.mp4");
    write(&old, 4096);
    aged(&old, 30);
    let clips = sandbox.join("clips");
    std::fs::create_dir_all(&clips).unwrap();
    let (_adopted, deleted) = kept::sweep_in(&root, &kept, &clips, 1 << 30);
    assert_eq!(deleted, 0);
    assert!(old.exists(), "an old unmarked copy whose clip is gone stays");
}

#[test]
fn clearing_editor_media_leaves_no_visuals_folder_behind() {
    let (_sandbox, root) = cache("visuals");
    let folder = root.join("visuals").join(A);
    for name in ["poster.jpg", "strip-0.jpg", "strip.json", "strip.json.1-2.part"] {
        write(&folder.join(name), 300);
    }
    let _ = tiers::clear_in(&root, CacheKind::EditorMedia);
    assert!(!folder.exists(), "the strip list goes with the frames it lists");
}

#[test]
fn a_blocked_cache_is_released_once_the_folders_are_apart() {
    let (sandbox, root) = cache("recheck");
    assert!(guard::check(std::slice::from_ref(&root), &[root.join("video")]).is_some());
    assert!(guard::blocked(&root).is_some());
    assert!(guard::check(std::slice::from_ref(&root), &[sandbox.join("clips")]).is_none());
    assert!(guard::blocked(&root).is_none(), "a folder change re-checks the block");
}

/// `\\localhost\<drive>$\...` for a local path, when the admin share answers; else `None`.
fn unc_alias(path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy().replace('/', "\\");
    let (drive, rest) = text.split_once(":\\")?;
    let alias = PathBuf::from(format!(r"\\localhost\{}$\{rest}", drive.to_ascii_lowercase()));
    alias.exists().then_some(alias)
}

#[test]
fn one_folder_reached_through_a_network_alias_is_still_the_cache() {
    let (_sandbox, root) = cache("alias");
    std::fs::create_dir_all(root.join("video")).unwrap();
    let real = std::fs::canonicalize(root.join("video")).map(|p| plain_path(&p)).unwrap();
    let Some(alias) = unc_alias(&real) else {
        eprintln!("skipped: the admin share is not reachable here");
        return;
    };
    assert!(guard::overlap(std::slice::from_ref(&root), &[alias]).is_some(), "\\\\localhost\\x$ and x: are one folder");
}

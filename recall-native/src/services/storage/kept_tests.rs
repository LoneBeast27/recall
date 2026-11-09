//! Adopting the engine's kept staged copies into the video tier (temp dirs only).
use super::*;
use crate::services::storage::evict;

const BUDGET: u64 = 1 << 30;

/// `%TEMP%\recall-kept-<tag>-<pid>-<nonce>`, removed when the guard drops, also when the test
/// panics (a failing run left `recall-kept-twice-*` behind, F-storage-2).
fn temp_dir(tag: &str) -> crate::test_temp::TempDir {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    crate::test_temp::TempDir::new(format!("recall-kept-{tag}-{}-{nonce}", std::process::id()))
}

/// (guard, cache root, clip folder, kept folder): keep the guard bound for the whole test.
fn dirs(tag: &str) -> (crate::test_temp::TempDir, PathBuf, PathBuf, PathBuf) {
    let base = temp_dir(tag);
    let (root, clips, kept) = (base.join("cache"), base.join("clips"), base.join("cache").join("staging").join("kept"));
    for dir in [&root, &clips, &kept] {
        std::fs::create_dir_all(dir).unwrap();
    }
    (base, root, clips, kept)
}

/// Marks `kept` the way the engine does (`recall/src/encoder/mover.rs`).
fn mark(kept: &Path) {
    std::fs::write(format!("{}:{KEPT_MARK}", kept.display()), b"").unwrap();
}

/// A clip in the clip folder and the engine's kept copy of it (`kept_len` bytes, the clip's
/// time, marked).
fn clip(clips: &Path, kept_dir: &Path, name: &str, len: usize, kept_len: usize) -> (PathBuf, PathBuf) {
    let (dest, kept) = (clips.join(name), kept_dir.join(name));
    std::fs::write(&dest, vec![7u8; len]).unwrap();
    std::fs::write(&kept, vec![7u8; kept_len]).unwrap();
    let saved = dest.metadata().unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(&kept).unwrap().set_modified(saved).unwrap();
    mark(&kept);
    (dest, kept)
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn a_kept_copy_becomes_the_clips_video_tier_entry() {
    let (_base, root, clips, kept_dir) = dirs("adopt");
    let (dest, kept) = clip(&clips, &kept_dir, "G__a.mp4", 5000, 5000);
    let target = adopt_in(&root, &dest, &kept, BUDGET).unwrap();
    let (id, _, _) = crate::services::storage::identity_of(&dest).unwrap();
    assert_eq!(target, root.join("video").join(format!("{id}.mp4")));
    assert_eq!(std::fs::read(&target).unwrap(), vec![7u8; 5000]);
    assert!(!kept.exists(), "renamed, not copied");
    assert_eq!(crate::services::storage::cached_video_in(&root, &dest).as_deref(), Some(target.to_string_lossy().as_ref()), "the open finds it");
    let index = evict::read_index(&root.join("video").join("lru.json"));
    assert!(index.contains_key(&format!("{id}.mp4")), "indexed for the budget: {index:?}");
    cleanup(&root);
}

#[test]
fn a_kept_copy_of_another_size_is_left_for_the_recorder() {
    // RI2-7: the clip differs from the copy, so the copy may be the whole one: the app keeps it.
    let (_base, root, clips, kept_dir) = dirs("size");
    let (dest, kept) = clip(&clips, &kept_dir, "G__b.mp4", 5000, 4000);
    assert!(adopt_in(&root, &dest, &kept, BUDGET).unwrap_err().contains("4000 bytes"));
    assert!(kept.exists(), "not the clip's copy: left for the recorder's check");
    assert!(crate::services::storage::cached_video_in(&root, &dest).is_none());
    cleanup(&root);
}

#[test]
fn a_clip_too_large_for_the_video_tier_is_not_adopted() {
    let (_base, root, clips, kept_dir) = dirs("large");
    let (dest, kept) = clip(&clips, &kept_dir, "G__c.mp4", 5000, 5000);
    // The video tier holds half the budget: 4000 bytes here.
    assert!(adopt_in(&root, &dest, &kept, 8000).unwrap_err().contains("larger"));
    assert!(!kept.exists());
    assert!(!root.join("video").read_dir().is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "mp4")))));
    cleanup(&root);
}

#[test]
fn a_clip_already_cached_drops_the_kept_copy() {
    let (_base, root, clips, kept_dir) = dirs("twice");
    let (dest, kept) = clip(&clips, &kept_dir, "G__d.mp4", 3000, 3000);
    let first = adopt_in(&root, &dest, &kept, BUDGET).unwrap();
    std::fs::write(&kept, vec![7u8; 3000]).unwrap();
    let saved = dest.metadata().unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(&kept).unwrap().set_modified(saved).unwrap();
    mark(&kept);
    assert_eq!(adopt_in(&root, &dest, &kept, BUDGET).unwrap(), first);
    assert!(!kept.exists() && first.exists());
    cleanup(&root);
}

#[test]
fn the_start_sweep_adopts_matches_and_reports_the_rest() {
    let (_base, root, clips, kept_dir) = dirs("sweep");
    let (good, good_kept) = clip(&clips, &kept_dir, "G__e.mp4", 2000, 2000);
    let (_, short_kept) = clip(&clips, &kept_dir, "G__f.mp4", 2000, 1500);
    let orphan = kept_dir.join("G__gone.mp4");
    std::fs::write(&orphan, vec![1u8; 900]).unwrap();
    mark(&orphan);
    std::fs::create_dir_all(kept_dir.join("sub")).unwrap();

    assert_eq!(sweep_in(&root, &kept_dir, &clips, BUDGET), (1, 2));
    assert!(crate::services::storage::cached_video_in(&root, &good).is_some(), "the matching copy is cached");
    assert!(!good_kept.exists(), "the adopted copy moved into the cache");
    // RI2-1: the app never deletes the rest; the recorder's start-up check decides.
    assert!(short_kept.exists() && orphan.exists(), "left for the recorder");
    assert!(kept_dir.join("sub").is_dir(), "folders are left alone");
    assert_eq!(sweep_in(&root, &kept_dir.join("missing"), &clips, BUDGET), (0, 0));
    cleanup(&root);
}

#[test]
fn only_a_file_in_a_kept_folder_may_be_adopted() {
    let (_base, root, clips, kept_dir) = dirs("guard");
    let (dest, kept) = clip(&clips, &kept_dir, "G__h.mp4", 2000, 2000);
    let allowed = [kept_dir.clone()];
    assert!(inside_kept(&kept, &allowed), "a copy in the kept folder is accepted");
    assert!(!inside_kept(&dest, &allowed), "a clip in the Raw folder is refused");
    let walked = kept_dir.join("..").join("..").join("..").join("clips").join("G__h.mp4");
    assert!(!inside_kept(&walked, &allowed), "a path that walks out of kept is refused");
    std::fs::create_dir_all(kept_dir.join("sub")).unwrap();
    std::fs::write(kept_dir.join("sub").join("deep.mp4"), b"x").unwrap();
    assert!(!inside_kept(&kept_dir.join("sub").join("deep.mp4"), &allowed), "only direct children");
    assert!(!inside_kept(&kept_dir.join("missing.mp4"), &allowed), "a missing file is refused");
    assert!(!inside_kept(&kept_dir, &allowed), "the folder itself is refused");
    // The public entry point leaves a refused clip exactly where it is.
    adopt_kept(dest.to_string_lossy().into_owned(), dest.to_string_lossy().into_owned());
    assert!(dest.is_file(), "adopt_kept never touches a clip outside the kept folders");
    cleanup(&root);
}

#[test]
fn a_kept_copy_is_never_deleted_while_its_clip_is_only_a_part() {
    // A power cut lost V:'s `.part` -> `.mp4` rename: the kept copy may be the only whole clip.
    let (_base, root, clips, kept_dir) = dirs("part");
    let kept = kept_dir.join("G__p.mp4");
    std::fs::write(&kept, vec![3u8; 1200]).unwrap();
    mark(&kept);
    std::fs::write(clips.join("G__p.mp4.part"), vec![3u8; 1200]).unwrap();
    assert_eq!(sweep_in(&root, &kept_dir, &clips, BUDGET), (0, 1), "nothing adopted, one left");
    assert!(kept.is_file(), "the engine re-moves it at its next start");
    cleanup(&root);
}

#[test]
fn a_kept_copy_is_never_deleted_while_the_clip_folder_cannot_be_read() {
    // RI-2: an asleep NAS (or a wrong folder list) reads as "no clip": the copy may be the only
    // whole clip after a lost rename, so a folder that can't be read keeps it.
    let (_base, root, clips, kept_dir) = dirs("asleep");
    let kept = kept_dir.join("G__n.mp4");
    std::fs::write(&kept, vec![4u8; 1300]).unwrap();
    mark(&kept);
    let asleep = clips.join("not-mounted");
    assert_eq!(sweep_in(&root, &kept_dir, &asleep, BUDGET), (0, 1), "a missing clip folder deletes nothing");
    std::fs::write(clips.join("a-file"), b"x").unwrap();
    assert_eq!(sweep_in(&root, &kept_dir, &clips.join("a-file"), BUDGET), (0, 1), "nor one that is not a folder");
    assert!(kept.is_file(), "the copy stays");
    cleanup(&root);
}

#[test]
fn a_readable_folder_without_the_clip_never_deletes_its_copy() {
    // RI2-1: the sweep's folder reads "no clip" while the clip's `.part` sits in another folder
    // Recall manages (the engine moved it there; a power cut lost the rename): the copy may be
    // the only whole clip, so the app never deletes it.
    let (_base, root, clips, kept_dir) = dirs("wrong-folder");
    let other = clips.parent().unwrap().join("old-recordings");
    std::fs::create_dir_all(&other).unwrap();
    let kept = kept_dir.join("G__w.mp4");
    std::fs::write(&kept, vec![8u8; 1400]).unwrap();
    mark(&kept);
    std::fs::write(other.join("G__w.mp4.part"), vec![8u8; 1400]).unwrap();
    assert_eq!(sweep_in(&root, &kept_dir, &clips, BUDGET), (0, 1), "nothing adopted, the copy is reported");
    assert!(kept.is_file(), "the copy stays for the recorder's check");
    cleanup(&root);
}

#[test]
fn a_kept_copy_of_a_clip_that_cannot_be_read_is_never_deleted() {
    // The app's delete guard matches the recorder's: present, readable, same size. A clip held
    // open with no sharing (being written, or a damaged file a tool holds) keeps its copy.
    use std::os::windows::fs::OpenOptionsExt;
    let (_base, root, clips, kept_dir) = dirs("unreadable");
    let (dest, kept) = clip(&clips, &kept_dir, "G__u.mp4", 5000, 5000);
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&dest).unwrap();
    assert!(adopt_in(&root, &dest, &kept, 8000).is_err(), "too large for the tier");
    drop(held);
    assert!(kept.exists(), "the clip could not be read: its copy stays");
    cleanup(&root);
}

#[test]
fn a_clip_with_other_bytes_never_takes_its_copy_into_the_cache() {
    // RI3-2: a full-length clip of zeros (a lost data write) looked whole by size and time; the
    // copy moved into the evictable cache. Now it is set aside in kept\leaked\.
    let (_base, root, clips, kept_dir) = dirs("zeroed");
    let (dest, kept) = clip(&clips, &kept_dir, "G__z.mp4", 5000, 5000);
    std::fs::write(&kept, vec![9u8; 5000]).unwrap();
    let saved = dest.metadata().unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(&kept).unwrap().set_modified(saved).unwrap();
    std::fs::write(&dest, vec![0u8; 5000]).unwrap();
    std::fs::File::options().write(true).open(&dest).unwrap().set_modified(saved).unwrap();
    assert!(adopt_in(&root, &dest, &kept, BUDGET).unwrap_err().contains("differ"));
    assert!(kept_dir.join("leaked").join("G__z.mp4").is_file(), "set aside, not deleted or cached");
    assert!(crate::services::storage::cached_video_in(&root, &dest).is_none());
    cleanup(&root);
}

#[test]
fn an_unknown_cache_size_takes_no_kept_copy() {
    // RI3-1: a damaged storage.json evicted nothing while every fresh clip still came in.
    let (_base, root, clips, kept_dir) = dirs("unknown");
    let (dest, kept) = clip(&clips, &kept_dir, "G__k.mp4", 5000, 5000);
    std::fs::write(root.join("storage.json"), b"\0\0\0").unwrap();
    assert!(adopt_in(&root, &dest, &kept, BUDGET).unwrap_err().contains("unknown"));
    assert!(kept.is_file(), "the copy waits, not deleted");
    assert!(crate::services::storage::cached_video_in(&root, &dest).is_none(), "nothing cached");
    cleanup(&root);
}

#[test]
fn a_clip_whose_middle_differs_keeps_its_copy() {
    // Head (64 KiB) and tail (1 MiB) match, one byte in the middle does not: the copy may be
    // the only whole version, so it is neither cached (evictable) nor deleted.
    let scratch = crate::test_temp::TempDir::new(format!("recall-kept-middle-{}", std::process::id()));
    let (root, clips, kept_dir) = (scratch.join("cache"), scratch.join("clips"), scratch.join("cache").join("staging").join("kept"));
    for dir in [&root, &clips, &kept_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut middle = vec![7u8; 3 << 20];
    middle[3 << 19] = 0;
    for (name, budget) in [("G__m.mp4", BUDGET), ("G__s.mp4", 8000)] {
        let (dest, kept) = clip(&clips, &kept_dir, name, 3 << 20, 3 << 20);
        let saved = dest.metadata().unwrap().modified().unwrap();
        std::fs::write(&dest, &middle).unwrap();
        std::fs::File::options().write(true).open(&dest).unwrap().set_modified(saved).unwrap();
        assert!(adopt_in(&root, &dest, &kept, budget).is_err(), "{name}");
        let kept_now = if kept.exists() { kept.clone() } else { kept_dir.join("leaked").join(name) };
        assert_eq!(std::fs::metadata(&kept_now).map(|m| m.len()).ok(), Some(3 << 20), "{name}: the copy is kept");
        assert!(crate::services::storage::cached_video_in(&root, &dest).is_none(), "{name}: not cached");
    }
}

#[test]
fn a_panicking_test_still_removes_its_temp_folder() {
    // F-storage-2: a failing test left its %TEMP% folder behind.
    let left = std::panic::catch_unwind(|| {
        let dir = temp_dir("panics");
        std::fs::write(dir.join("clip.mp4"), vec![1u8; 4096]).unwrap();
        std::panic::panic_any(dir.to_path_buf());
    })
    .unwrap_err()
    .downcast::<PathBuf>()
    .unwrap();
    let gone = !left.exists();
    let _ = std::fs::remove_dir_all(&*left);
    assert!(gone, "{} was left behind", left.display());
}

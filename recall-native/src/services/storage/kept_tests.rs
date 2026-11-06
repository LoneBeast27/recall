//! Adopting the engine's kept staged copies into the video tier (temp dirs only).
use super::*;
use crate::services::storage::evict;

const BUDGET: u64 = 1 << 30;

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("recall-kept-{tag}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// (cache root, clip folder, kept folder)
fn dirs(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = temp_dir(tag);
    let (root, clips, kept) = (base.join("cache"), base.join("clips"), base.join("cache").join("staging").join("kept"));
    for dir in [&root, &clips, &kept] {
        std::fs::create_dir_all(dir).unwrap();
    }
    (root, clips, kept)
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
    let (root, clips, kept_dir) = dirs("adopt");
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
fn a_kept_copy_of_another_size_is_deleted() {
    let (root, clips, kept_dir) = dirs("size");
    let (dest, kept) = clip(&clips, &kept_dir, "G__b.mp4", 5000, 4000);
    assert!(adopt_in(&root, &dest, &kept, BUDGET).unwrap_err().contains("4000 bytes"));
    assert!(!kept.exists(), "a copy that is not the clip goes");
    assert!(crate::services::storage::cached_video_in(&root, &dest).is_none());
    cleanup(&root);
}

#[test]
fn a_clip_too_large_for_the_video_tier_is_not_adopted() {
    let (root, clips, kept_dir) = dirs("large");
    let (dest, kept) = clip(&clips, &kept_dir, "G__c.mp4", 5000, 5000);
    // The video tier holds half the budget: 4000 bytes here.
    assert!(adopt_in(&root, &dest, &kept, 8000).unwrap_err().contains("larger"));
    assert!(!kept.exists());
    assert!(!root.join("video").read_dir().is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "mp4")))));
    cleanup(&root);
}

#[test]
fn a_clip_already_cached_drops_the_kept_copy() {
    let (root, clips, kept_dir) = dirs("twice");
    let (dest, kept) = clip(&clips, &kept_dir, "G__d.mp4", 3000, 3000);
    let first = adopt_in(&root, &dest, &kept, BUDGET).unwrap();
    std::fs::write(&kept, vec![7u8; 3000]).unwrap();
    mark(&kept);
    assert_eq!(adopt_in(&root, &dest, &kept, BUDGET).unwrap(), first);
    assert!(!kept.exists() && first.exists());
    cleanup(&root);
}

#[test]
fn the_start_sweep_adopts_matches_and_deletes_the_rest() {
    let (root, clips, kept_dir) = dirs("sweep");
    let (good, good_kept) = clip(&clips, &kept_dir, "G__e.mp4", 2000, 2000);
    let (_, short_kept) = clip(&clips, &kept_dir, "G__f.mp4", 2000, 1500);
    let orphan = kept_dir.join("G__gone.mp4");
    std::fs::write(&orphan, vec![1u8; 900]).unwrap();
    mark(&orphan);
    std::fs::create_dir_all(kept_dir.join("sub")).unwrap();

    assert_eq!(sweep_in(&root, &kept_dir, &clips, BUDGET), (1, 2));
    assert!(crate::services::storage::cached_video_in(&root, &good).is_some(), "the matching copy is cached");
    assert!(!good_kept.exists() && !short_kept.exists() && !orphan.exists());
    assert!(kept_dir.join("sub").is_dir(), "folders are left alone");
    assert_eq!(sweep_in(&root, &kept_dir.join("missing"), &clips, BUDGET), (0, 0));
    cleanup(&root);
}

#[test]
fn only_a_file_in_a_kept_folder_may_be_adopted() {
    let (root, clips, kept_dir) = dirs("guard");
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

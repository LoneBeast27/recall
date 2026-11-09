//! Owned wins over linked: Recall's own clips under a linked parent, after a folder change, and
//! in a startup `-OutDir` override folder go to the Recycle Bin; another recorder's clips beside
//! them are only hidden and never touched.
use std::path::{Path, PathBuf};

use super::*;
use crate::services::library::folders::Folders;

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn clip(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"clip").unwrap();
    path
}

#[test]
fn an_old_folder_list_with_videos_linked_keeps_recalls_clips_its_own() {
    // Trigger 1 (and the parent-link case): recordings in Videos\Recall, Videos linked.
    let root = crate::test_temp::TempDir::sandbox("owned-under-link");
    let (videos, recordings) = (root.join("Videos"), root.join("Videos").join("Recall"));
    let own = clip(&recordings, "VALORANT__2026-11-01__10-00-00.mp4");
    let foreign = clip(&videos.join("Moments"), "moment.mp4");
    let folders = Folders { recordings: Some(text(&recordings)), imported: vec![text(&videos)], ..Folders::default() };
    let (imported, owned) = partition_for_delete(&[own.clone(), foreign.clone()], &folders);
    assert_eq!((imported, owned), (vec![foreign.clone()], vec![own.clone()]), "own clip to the bin, the foreign one hidden");
    assert_eq!(imported_count(&[own.clone(), foreign.clone()], &folders), 1, "the dialog counts what the delete does");
    let exports = crate::backend::checked_export_dir(&own, &folders).expect("own clips export");
    assert_eq!(exports, recordings.join("exports"));
    assert_eq!(crate::backend::checked_export_dir(&foreign, &folders).unwrap(), recordings.join("exports"));
    // The foreign clip: Delete only hides it, the file stays byte for byte.
    let report = delete_or_hide(std::slice::from_ref(&foreign), &folders);
    assert_eq!((report.hidden, report.recycled), (1, 0));
    assert_eq!(std::fs::read(&foreign).unwrap(), b"clip");
    assert_eq!(std::fs::read_dir(videos.join("Moments")).unwrap().count(), 1, "nothing written beside it");
}

#[test]
fn releasing_a_former_folder_that_holds_the_recordings_folder_is_refused() {
    // Trigger 2: D:\Clips -> D:\Clips\Recall, then remove D:\Clips from Folders Recall manages.
    let root = crate::test_temp::TempDir::sandbox("release-parent");
    let (old, new) = (root.join("Clips"), root.join("Clips").join("Recall"));
    let mut folders = Folders { recordings: Some(text(&old)), ..Folders::default() };
    crate::views::folders_sync::retarget(&mut folders, &text(&new), None).unwrap();
    let _ = folders.release(&text(&old));
    assert!(folders.former_recordings.iter().any(|f| *f == text(&old)), "D:\\Clips stays managed");
    let (own_new, own_old) = (clip(&new, "a.mp4"), clip(&old, "b.mp4"));
    let (imported, owned) = partition_for_delete(&[own_new.clone(), own_old.clone()], &folders);
    assert!(imported.is_empty(), "{imported:?} would only be hidden");
    assert_eq!(owned, vec![own_new, own_old]);
}

#[test]
fn a_clip_recorded_into_an_override_folder_recycles_and_a_foreign_one_does_not() {
    let root = crate::test_temp::TempDir::sandbox("override-owned");
    let tmp = root.join("Tmp");
    let (recorded, foreign) = (clip(&tmp, "VALORANT__2026-11-02__10-00-00.mp4"), clip(&tmp, "other.mp4"));
    super::super::recorded::note(&text(&recorded));
    let folders = Folders { recordings: Some(text(&root.join("Recall"))), ..Folders::default() };
    let (imported, owned) = partition_for_delete(&[recorded.clone(), foreign.clone()], &folders);
    assert_eq!(owned, vec![recorded.clone()], "the clip Recall recorded goes to the bin");
    assert_eq!(imported, vec![foreign], "never widened to other files in that folder");
    super::super::recorded::forget(std::slice::from_ref(&recorded));
    assert!(!super::super::recorded::contains(&recorded));
}

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
fn an_owned_clip_named_by_its_verbatim_path_still_goes_to_the_bin() {
    // The editor holds a clip by its canonical path (`\\?\Y:\...`); the folder list is plain.
    let root = crate::test_temp::TempDir::sandbox("owned-verbatim");
    let own = clip(&root.join("Recall"), "undone.mp4");
    // The folder list holds the plain form (the sandbox path has a `..` in it).
    let recordings = crate::services::storage::plain_path(&std::fs::canonicalize(root.join("Recall")).unwrap());
    let verbatim = std::fs::canonicalize(&own).unwrap();
    assert!(text(&verbatim).starts_with(r"\\?\"), "{verbatim:?}");
    let folders = Folders { recordings: Some(text(&recordings)), imported: vec![text(&root)], ..Folders::default() };
    let (imported, owned) = partition_for_delete(std::slice::from_ref(&verbatim), &folders);
    assert_eq!((imported, owned), (Vec::new(), vec![verbatim.clone()]), "Delete recycles, never only hides");
    let exports = crate::backend::checked_export_dir(&verbatim, &folders).expect("own clips export");
    assert_eq!(crate::services::storage::plain_path(&exports), recordings.join("exports"));
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

fn marked(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    crate::services::library::folders::rule::write_marker(dir).unwrap();
    dir.to_path_buf()
}

fn listed(recordings: &Path, imported: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = crate::backend::list_from(recordings, imported, &[]).unwrap().0.into_iter().map(|c| c.path).collect();
    paths.sort();
    paths
}

/// "Sort clips into game folders": a recorder clip in a game folder Recall made is Recall's own
/// (Delete recycles it, edits sit beside it); another video in that folder stays foreign.
#[test]
fn a_game_folder_clip_is_recalls_own() {
    let root = crate::test_temp::TempDir::sandbox("owned-game-folder");
    let recordings = root.join("Recall");
    let game = marked(&recordings.join("Rocket League"));
    let own = clip(&game, "Rocket-League__2026-11-01__10-00-00-000__ab.mp4");
    let other = clip(&game, "holiday.mp4");
    let folders = Folders { recordings: Some(text(&recordings)), ..Folders::default() };
    assert!(imports::is_owned(&own, &recordings, &[], &[]));
    assert!(!imports::is_owned(&other, &recordings, &[], &[]));
    let (imported, owned) = partition_for_delete(std::slice::from_ref(&own), &folders);
    assert_eq!((imported, owned), (Vec::<PathBuf>::new(), vec![own.clone()]));
    assert_eq!(crate::backend::checked_export_dir(&own, &folders).unwrap(), game.join("exports"));
    assert_eq!(listed(&recordings, &[]), vec![text(&own)], "the library finds it, and only it");
    let roots = crate::backend::clip_roots_from(&folders);
    assert!(roots.iter().any(|r| r.path == game && !r.recursive), "its folder is watched");
}

/// Option off or on: `<recordings>\Keepers\` (the user's folder) holding a Recall-named clip is
/// not listed, not owned, never recycled as Recall's.
#[test]
fn an_unmarked_folder_with_recall_named_clips_stays_the_users() {
    let root = crate::test_temp::TempDir::sandbox("owned-unmarked");
    let recordings = root.join("Recall");
    let keeper = clip(&recordings.join("Keepers"), "VALORANT__2026-11-01__10-00-00-000__ab.mp4");
    let folders = Folders { recordings: Some(text(&recordings)), ..Folders::default() };
    assert!(!imports::is_owned(&keeper, &recordings, &[], &[]));
    assert!(listed(&recordings, &[]).is_empty());
    assert!(!crate::backend::clip_roots_from(&folders).iter().any(|r| r.path == recordings.join("Keepers")));
    let (_, owned) = partition_for_delete(std::slice::from_ref(&keeper), &folders);
    assert!(owned.is_empty(), "never sent to the Recycle Bin as Recall's");
}

/// Linked beats game folder: a linked `<recordings>\Valorant\` (even with Recall's marker)
/// holding Recall-named clips is the link's, read-only.
#[test]
fn a_linked_game_folder_is_not_owned() {
    let root = crate::test_temp::TempDir::sandbox("owned-linked-game");
    let recordings = root.join("Recall");
    let valorant = marked(&recordings.join("Valorant"));
    let linked_clip = clip(&valorant, "VALORANT__2026-11-01__10-00-00-000__ab.mp4");
    let folders = Folders { recordings: Some(text(&recordings)), imported: vec![text(&valorant)], ..Folders::default() };
    assert!(!imports::is_owned(&linked_clip, &recordings, &[], &folders.imported));
    assert!(imports::is_imported(&text(&linked_clip), &recordings, &folders.imported, &[]));
    let (imported, owned) = partition_for_delete(std::slice::from_ref(&linked_clip), &folders);
    assert_eq!((imported, owned), (vec![linked_clip.clone()], Vec::<PathBuf>::new()), "hidden, never recycled");
    let roots = crate::backend::clip_roots_from(&folders);
    assert!(!roots.iter().any(|r| r.path == valorant && !r.recursive), "watched only as its link");
    // A link above the recordings folder (Videos linked): Recall's game-folder clips stay Recall's.
    assert!(imports::is_owned(&linked_clip, &recordings, &[], &[text(&root)]));
}

/// A junction game folder carrying a marker is skipped by the library list and the watch roots.
#[test]
fn a_junction_game_folder_is_skipped() {
    let root = crate::test_temp::TempDir::sandbox("owned-junction");
    let recordings = root.join("Recall");
    std::fs::create_dir_all(&recordings).unwrap();
    let target = marked(&root.join("elsewhere"));
    clip(&target, "Apex-Legends__2026-11-01__10-00-00-000__ab.mp4");
    let link = recordings.join("Apex Legends");
    if !crate::services::library::folders::rule::junction_for_test(&target, &link) {
        eprintln!("skipped: could not make a junction");
        return;
    }
    let folders = Folders { recordings: Some(text(&recordings)), ..Folders::default() };
    assert!(listed(&recordings, &[]).is_empty());
    assert!(!crate::backend::clip_roots_from(&folders).iter().any(|r| r.path == link));
    assert!(!imports::is_owned(&link.join("Apex-Legends__2026-11-01__10-00-00-000__ab.mp4"), &recordings, &[], &[]));
}

/// A marked game folder the user hid is skipped by the library, as by the storage limit.
#[test]
fn a_hidden_game_folder_is_skipped() {
    let root = crate::test_temp::TempDir::sandbox("owned-hidden");
    let recordings = root.join("Recall");
    let game = marked(&recordings.join("Rocket League"));
    clip(&game, "Rocket-League__2026-11-01__10-00-00-000__ab.mp4");
    assert_eq!(listed(&recordings, &[]).len(), 1);
    if !crate::services::library::folders::rule::hide_for_test(&game) {
        eprintln!("skipped: could not hide the folder");
        return;
    }
    assert!(listed(&recordings, &[]).is_empty());
}

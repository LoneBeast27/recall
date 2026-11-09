//! The cache deletes only what Recall made there: a user's mp4 or .part placed in any cache
//! folder survives every sweep, Recall's own copies are still evicted, and a cache that
//! overlaps a clip folder deletes nothing. Sandbox folders under the worktree's `.tmp` only.
use std::os::windows::fs::FileTimesExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;
use crate::test_temp::TempDir;

const BUDGET: u64 = 1 << 30;
/// A Recall-shaped name (16 hex digits) the user put there by hand.
const HEX_NAME: &str = "0123456789abcdef";

fn write(path: &Path, len: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![9u8; len]).unwrap();
}

/// Created and modified `hours` ago (the stale-part sweep reads the creation time).
fn age(path: &Path, hours: u64) {
    let at = SystemTime::now() - Duration::from_secs(hours * 3600);
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(at).set_created(at)).unwrap();
}

/// (sandbox, cache root, clip folder)
fn layout(tag: &str) -> (TempDir, PathBuf, PathBuf) {
    let sandbox = TempDir::sandbox(format!("recall-owned-{tag}"));
    let (root, clips) = (sandbox.join("cache"), sandbox.join("clips"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&clips).unwrap();
    (sandbox, root, clips)
}

/// Recall's own video copy of a clip in `clips`.
fn own_copy(root: &Path, clips: &Path, name: &str) -> PathBuf {
    let source = clips.join(name);
    write(&source, 64 << 10);
    video::cache_in(root, &source, BUDGET).unwrap()
}

/// What a user (or a mistaken folder pick) left in every cache folder.
fn user_files(root: &Path) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = [
        "video/My clip.mp4",
        &format!("video/{HEX_NAME}.mp4"),
        "light/My clip.mp4",
        &format!("light/{HEX_NAME}.mp4"),
        "editor/My clip.mp4",
        &format!("editor/{HEX_NAME}/My clip.mp4"),
        "thumbs/My clip.mp4",
        &format!("visuals/{HEX_NAME}/My clip.mp4"),
        "staging/kept/My clip.mp4",
    ]
    .iter()
    .map(|rel| root.join(rel))
    .collect();
    for file in &files {
        write(file, 4096);
    }
    let parts = [root.join("video").join("My clip.mp4.part"), root.join("video").join("G__2026-10-22__14-05-09__rec.mp4.part")];
    for part in &parts {
        write(part, 4096);
        age(part, 2);
    }
    files.into_iter().chain(parts).collect()
}

/// Every sweep and eviction the cache runs, at a budget of zero.
fn sweep_everything(root: &Path, clips: &Path) {
    budget::sweep_old_stems(root, &[]);
    budget::enforce(root, 0, 0);
    let _ = budget::room_below(root, 0, 0, 0, f64::MAX);
    for kind in CacheKind::ALL {
        let _ = tiers::clear_in(root, kind);
    }
    video::remove_stale_parts(&root.join("video"));
    kept::sweep_in(root, &root.join("staging").join("kept"), clips, BUDGET);
}

#[test]
fn a_user_mp4_and_part_in_every_cache_folder_survive_every_sweep() {
    let (_sandbox, root, clips) = layout("survive");
    let copy = own_copy(&root, &clips, "G__a.mp4");
    // Recall's own: a light copy, a stale part of a crashed copy, editor stems, a poster.
    let light = root.join("light").join("1111111111111111.mp4");
    write(&light, 4096);
    mark_owned(&root, "light/1111111111111111.mp4");
    let stale = root.join("video").join("2222222222222222.mp4.123-456.part");
    write(&stale, 4096);
    age(&stale, 2);
    mark_owned(&root, "video/2222222222222222.mp4");
    let stems = root.join("editor").join(HEX_NAME);
    write(&stems.join("clip.json"), 200);
    write(&stems.join("audio-0.wav"), 4096);
    let poster = root.join("visuals").join(HEX_NAME).join("poster.jpg");
    write(&poster, 4096);
    let user = user_files(&root);
    sweep_everything(&root, &clips);
    for file in &user {
        assert!(file.exists(), "a user file was deleted: {}", file.display());
    }
    assert!(clips.join("G__a.mp4").exists(), "the clip itself is never touched");
    for own in [&copy, &light, &stale, &stems.join("audio-0.wav"), &poster] {
        assert!(!own.exists(), "Recall's own file was not evicted: {}", own.display());
    }
}

#[test]
fn a_user_mp4_in_a_cache_made_before_the_marker_is_left_alone() {
    let (_sandbox, root, clips) = layout("legacy");
    // A cache from before `owned.json`: Recall's names only, plus the user's own clip.
    let old_copy = root.join("video").join("3333333333333333.mp4");
    write(&old_copy, 4096);
    let old_part = root.join("video").join("3333333333333333.mp4.77-88.part");
    write(&old_part, 4096);
    age(&old_part, 2);
    let user = root.join("video").join("VALORANT__2026-10-22__14-05-09-275__ab12.mp4");
    write(&user, 4096);
    sweep_everything(&root, &clips);
    assert!(user.exists(), "a clip-named file in the cache is the user's");
    assert!(!old_copy.exists() && !old_part.exists(), "Recall's exact names are adopted once and evicted");
}

#[test]
fn a_kept_copy_the_engine_did_not_mark_is_never_deleted() {
    let (_sandbox, root, clips) = layout("kept");
    let kept_dir = root.join("staging").join("kept");
    let unmarked = kept_dir.join("Mine.mp4");
    write(&unmarked, 4096);
    // The engine's own leftover (its clip was deleted while the app was closed).
    let marked = kept_dir.join("G__gone.mp4");
    write(&marked, 4096);
    std::fs::write(format!("{}:{}", marked.display(), kept::KEPT_MARK), b"").unwrap();
    kept::sweep_in(&root, &kept_dir, &clips, BUDGET);
    assert!(unmarked.exists(), "unmarked: not the engine's, left alone");
    // RI2-1: the recorder's start-up check (it knows its clip folder) removes it, not the app.
    assert!(marked.exists(), "the engine's marked copy with no clip is left for the recorder");
}

#[test]
fn a_blocked_cache_deletes_nothing_not_even_its_own_copies() {
    let (_sandbox, root, clips) = layout("blocked");
    let copy = own_copy(&root, &clips, "G__b.mp4");
    guard::block(&root, "test: the clip folder is inside the cache".into());
    sweep_everything(&root, &clips);
    assert!(copy.exists(), "a cache that overlaps a clip folder evicts nothing");
    assert!(guard::blocked(&root).is_some());
}

#[test]
fn a_clip_folder_inside_the_cache_or_the_cache_inside_one_is_refused() {
    let (sandbox, root, _clips) = layout("overlap");
    let cache = vec![root.clone()];
    assert!(guard::overlap(&cache, &[root.join("video")]).is_some(), "clip folder inside the cache");
    assert!(guard::overlap(&cache, std::slice::from_ref(&root)).is_some(), "the same folder");
    assert!(guard::overlap(&cache, &[sandbox.to_path_buf()]).is_some(), "the cache inside a clip folder");
    let shouted = PathBuf::from(root.to_string_lossy().to_uppercase()).join("Light");
    assert!(guard::overlap(&cache, &[shouted]).is_some(), "case does not hide it");
    assert!(guard::overlap(&cache, &[sandbox.join("clips")]).is_none(), "a sibling folder is fine");
    assert!(guard::overlap(&cache, &[sandbox.join("cache2")]).is_none(), "a name prefix is not a parent");
    assert!(guard::overlap(&cache, &[]).is_none());
    let message = guard::overlap(&cache, &[root.join("video")]).unwrap_or_default();
    assert!(message.contains("video"), "the message names the folder: {message}");
}

#[test]
fn the_start_check_blocks_a_cache_that_holds_the_recordings_folder() {
    let (_sandbox, root, _clips) = layout("start");
    // The recordings folder was set to the cache's own video folder.
    let video = root.join("video");
    let clip = video.join("VALORANT__2026-10-22__14-05-09-275__ab12.mp4");
    write(&clip, 4096);
    let crashed = video.join("VALORANT__2026-10-22__15-00-00__rec.mp4.part");
    write(&crashed, 4096);
    age(&crashed, 2);
    let message = guard::check(std::slice::from_ref(&root), std::slice::from_ref(&video)).expect("the overlap is refused");
    assert!(message.contains("deletes nothing"), "the message says what the app does instead: {message}");
    sweep_everything(&root, &video);
    assert!(clip.exists() && crashed.exists(), "nothing in the clip folder is touched, the crashed recording waits for recovery");
    assert!(video::copy_in(&root, &clip, BUDGET, &|_| true, &video::FULL_SPEED).is_err(), "and no new copies land in it");
}

#[test]
fn a_clip_open_in_another_window_keeps_its_video_copy() {
    // "In use" was only the editor's own clip (KEEP) and files Windows refused to delete; Recall's
    // readers share delete, so the copy another window plays from could be evicted under it.
    let (_sandbox, root, clips) = layout("open-elsewhere");
    let copy = own_copy(&root, &clips, "G__2026-11-10__10-00-00.mp4");
    let source = clips.join("G__2026-11-10__10-00-00.mp4");
    let open = crate::services::library::open_clips::exporting(&source.to_string_lossy());
    budget::enforce(&root, 0, 0);
    let _ = budget::room_below(&root, 0, 0, 0, f64::MAX);
    assert!(copy.is_file(), "the copy of a clip open in Recall was evicted");
    drop(open);
    budget::enforce(&root, 0, 0);
    assert!(!copy.exists(), "once it closes, it may go");
}

/// `link` as a directory junction to `target` (`mklink /J`, no admin rights needed).
fn junction(link: &Path, target: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().unwrap();
    assert!(made.status.success(), "mklink /J: {}", String::from_utf8_lossy(&made.stderr));
}

#[test]
fn a_junction_tier_folder_is_never_adopted_from_or_deleted_through() {
    // video\ (or editor\<id>) a junction into a user's folder: the first load adopted the
    // Recall-named files there into owned.json, and eviction then deleted them through it.
    let (sandbox, root, _clips) = layout("junction");
    let (target, stems) = (sandbox.join("user-videos"), sandbox.join("user-stems"));
    write(&target.join(format!("{HEX_NAME}.mp4")), 4096);
    write(&stems.join("clip.json"), 64);
    write(&stems.join("a.flac"), 4096);
    junction(&root.join("video"), &target);
    junction(&root.join("editor").join(HEX_NAME), &stems);
    assert!(!owned::listed(&root).contains(&format!("video/{HEX_NAME}.mp4")), "adopted through a junction");
    assert!(!owned::remove(&root, "video", &format!("{HEX_NAME}.mp4")));
    budget::enforce(&root, 0, 0);
    assert_eq!(super::unit_files::remove_engine_folder(&root.join("editor").join(HEX_NAME)), 0);
    for file in [target.join(format!("{HEX_NAME}.mp4")), stems.join("clip.json"), stems.join("a.flac")] {
        assert!(file.is_file(), "deleted through a junction: {}", file.display());
    }
    // The junctions go first, so the sandbox cleanup never walks into their targets.
    let _ = std::fs::remove_dir(root.join("video"));
    let _ = std::fs::remove_dir(root.join("editor").join(HEX_NAME));
}

#[test]
fn a_clip_open_in_another_window_keeps_its_editor_stems() {
    // kept() covered an open clip's video copy, frames and thumbnail, not its stem folders
    // (editor\<id>): the stems another window's editor plays from could be evicted under it.
    let (_sandbox, root, clips) = layout("stems-elsewhere");
    let source = clips.join("G__2026-11-10__11-00-00.mp4");
    write(&source, 64 << 10);
    let (id, _, _) = identity_of(&source).unwrap();
    let stems = root.join("editor").join(HEX_NAME);
    write(&stems.join("clip.json"), 200);
    write(&stems.join("audio-0.flac"), 4096);
    // Another window prepared it (owner recorded, not this process's editor clip).
    budget::note_editor_media(&root, &id, &[stems.clone()], false);
    let open = crate::services::library::open_clips::exporting(&source.to_string_lossy());
    budget::enforce(&root, 0, 0);
    let _ = budget::room_below(&root, 0, 0, 0, f64::MAX);
    assert!(stems.join("audio-0.flac").is_file(), "the stems of a clip open in Recall were evicted");
    drop(open);
    budget::enforce(&root, 0, 0);
    assert!(!stems.join("audio-0.flac").exists(), "once it closes, they may go");
}

//! Staging folder choice, the volume check and start-up recovery (temp dirs only).

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-staging-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn staging_folder_comes_from_the_env_or_the_exe_folder() {
    let exe = Path::new(r"F:\Programs\Recall\recall.exe");
    let default = PathBuf::from(r"F:\Programs\Recall\cache\staging");
    assert_eq!(configured(None, Some(exe)), Some(default.clone()));
    assert_eq!(configured(Some("  "), Some(exe)), Some(default.clone()), "blank = default");
    assert_eq!(configured(Some(r"D:\fast\stage"), Some(exe)), Some(PathBuf::from(r"D:\fast\stage")));
    assert_eq!(configured(Some("off"), Some(exe)), None);
    assert_eq!(configured(Some(" OFF "), Some(exe)), None);
    assert_eq!(configured(None, None), None, "no exe path, no default");
    // Off still recovers what an earlier run left in the default folder.
    assert_eq!(recovery_dir(Some("off"), Some(exe)), Some(default));
    assert_eq!(recovery_dir(Some(r"D:\s"), Some(exe)), Some(PathBuf::from(r"D:\s")));
}

#[test]
fn volumes_compare_by_drive_letter_or_share() {
    let same = |a: &str, b: &str| same_volume(Path::new(a), Path::new(b));
    assert!(!same(r"F:\Programs\Recall\cache\staging", r"E:\Clips"));
    assert!(same(r"f:\a", r"F:\b\c"), "drive letters are case-insensitive");
    assert!(same(r"\\?\E:\x", r"E:\Clips"), "verbatim and plain forms match");
    assert!(same(r"\\nas\clips\a", r"\\NAS\Clips\b"));
    assert!(!same(r"\\nas\clips\a", r"\\nas\other\a"));
    assert!(!same(r"\\nas\clips", r"E:\clips"));
}

#[test]
fn a_save_is_staged_only_across_volumes() {
    let stage = Path::new(r"F:\Recall\cache\staging");
    let clip = Path::new(r"E:\Clips\G__2026-10-23__10-00-00-000__ab.mp4");
    assert_eq!(
        staged_path(stage, clip, same_volume),
        Some(PathBuf::from(r"F:\Recall\cache\staging\G__2026-10-23__10-00-00-000__ab.mp4"))
    );
    assert_eq!(staged_path(stage, Path::new(r"F:\clips\G.mp4"), same_volume), None, "same volume: in place");
    assert_eq!(staged_path(stage, Path::new(r"E:\"), same_volume), None, "no file name");
}

#[test]
fn recovery_queues_finished_clips_and_deletes_only_stale_mp4_parts() {
    let (stage, out) = (temp_dir("recover"), PathBuf::from(r"E:\never-touched"));
    let older = stage.join("A__2026-10-22__09-00-00-000__aa.mp4");
    std::fs::write(&older, b"a").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let newer = stage.join("B__2026-10-22__09-05-00-000__bb.MP4");
    std::fs::write(&newer, b"b").unwrap();
    let part = stage.join("C__2026-10-22__09-06-00-000__cc.mp4.part");
    std::fs::write(&part, b"half").unwrap();
    std::fs::write(stage.join("notes.txt"), b"keep").unwrap();
    std::fs::write(stage.join("other.part"), b"keep").unwrap();
    std::fs::create_dir_all(stage.join("sub.mp4")).unwrap();

    let found = recover(&stage, &out);
    assert_eq!(
        found.moves,
        vec![
            (older.clone(), out.join("A__2026-10-22__09-00-00-000__aa.mp4")),
            (newer.clone(), out.join("B__2026-10-22__09-05-00-000__bb.MP4")),
        ],
        "oldest first"
    );
    assert_eq!(found.removed, vec![part.clone()]);
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    assert!(!part.exists());
    for kept in ["notes.txt", "other.part", "sub.mp4"] {
        assert!(stage.join(kept).exists(), "{kept} must be left alone");
    }
    assert!(older.exists() && newer.exists(), "recovery only queues; the mover moves");
    assert!(recover(&stage.join("missing"), &out).moves.is_empty());
    let _ = std::fs::remove_dir_all(&stage);
}

#[test]
fn recovery_never_requeues_copies_kept_for_the_app() {
    let (stage, out) = (temp_dir("kept"), PathBuf::from(r"V:
ever-touched"));
    let kept_dir = stage.join(super::mover::KEPT_DIR);
    std::fs::create_dir_all(&kept_dir).unwrap();
    let kept = kept_dir.join("A__2026-10-22__09-00-00-000__aa.mp4");
    std::fs::write(&kept, b"moved already").unwrap();
    let part = kept_dir.join("B__2026-10-22__09-01-00-000__bb.mp4.part");
    std::fs::write(&part, b"not ours").unwrap();
    let found = recover(&stage, &out);
    assert!(found.moves.is_empty(), "{:?}", found.moves);
    assert!(found.removed.is_empty() && found.errors.is_empty());
    assert!(kept.exists() && part.exists(), "kept copies are the app's to adopt or delete");
    let _ = std::fs::remove_dir_all(&stage);
}

#[test]
fn an_uncreatable_staging_folder_falls_back_to_writing_in_place() {
    let dir = temp_dir("blocked");
    let file = dir.join("not-a-folder");
    std::fs::write(&file, b"x").unwrap();
    let (events, sink) = Events::capture(true);
    let cfg = mover::Config { background: false, ..mover::Config::default() };
    let (queue, control) = mover::spawn(events.clone(), cfg, Vec::new()).unwrap();
    let stage = Stage { dir: file.join("staging"), queue, same_volume: |_, _| false };
    assert_eq!(stage.target(r"E:\c\G.mp4", &events), None);
    let logs = sink.lock().unwrap().clone();
    assert!(logs.iter().any(|l| l.contains("writing the clip in place")), "{logs:?}");
    let ok = Stage { dir: dir.join("staging"), ..stage };
    assert_eq!(ok.target(r"E:\c\G.mp4", &events), Some(dir.join("staging").join("G.mp4")));
    assert!(dir.join("staging").is_dir());
    assert!(control.shutdown(std::time::Duration::from_secs(1)));
    let _ = std::fs::remove_dir_all(&dir);
}

static START_VOLUME_LOOKUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn counted_same_volume(_: &Path, _: &Path) -> bool {
    START_VOLUME_LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    false
}

#[test]
fn engine_start_asks_no_volume_question() {
    // A clip folder on a sleeping NAS must not hold the encoder's start: the
    // same-volume answer is asked per save, on the saver thread.
    let (stage_dir, out) = (temp_dir("start-stage"), temp_dir("start-out"));
    let (events, _sink) = Events::capture(true);
    let mover = mover::Config { background: false, same_volume: counted_same_volume, ..mover::Config::default() };
    let setup = Setup { dir: Some(stage_dir.clone()), recover: None, out_dir: out.clone(), mover, refused: Vec::new() };
    let (stage, control) = start(&events, setup);
    assert!(stage.is_some());
    assert_eq!(START_VOLUME_LOOKUPS.load(std::sync::atomic::Ordering::SeqCst), 0, "start looked up a volume");
    assert!(control.unwrap().shutdown(std::time::Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage_dir), std::fs::remove_dir_all(&out));
}

#[test]
fn recovery_leaves_a_crashed_long_recording_part_for_its_own_recovery() {
    // The clip folder pointed at the staging folder: a long recording's `.part` there must
    // wait for `longrec::recover_parts`, which renames it; it is never a stale mux file.
    let stage = sandbox("longrec");
    let part = stage.join("G__2026-10-22__14-05-09__rec.mp4.part");
    std::fs::write(&part, b"fragments").unwrap();
    let mux = stage.join("G__2026-10-22__14-05-09-275__ab12.mp4.part");
    std::fs::write(&mux, b"half a mux").unwrap();
    let found = recover(&stage, &stage);
    assert!(part.exists(), "the long recording waits for its recovery");
    assert_eq!(found.removed, vec![mux.clone()], "only the saver's own stale mux file goes");
    let _ = std::fs::remove_dir_all(&stage);
}

#[test]
fn a_staging_folder_inside_or_around_a_clip_folder_is_refused() {
    let base = sandbox("refuse");
    let (clips, other) = (base.join("Clips"), base.join("Imported"));
    let dirs = vec![clips.clone(), other.clone()];
    assert!(refused(&clips, &dirs).is_some(), "the clip folder itself");
    assert!(refused(&clips.join("stage"), &dirs).is_some(), "inside the clip folder");
    assert!(refused(&base, &dirs).is_some(), "around the clip folder");
    assert!(refused(&other.join("x"), &dirs).is_some(), "inside an imported folder");
    let upper = PathBuf::from(clips.to_string_lossy().to_uppercase());
    assert!(refused(&upper, &dirs).is_some(), "case does not hide it");
    assert!(refused(&base.join("Clips2"), &dirs).is_none(), "a name prefix is not a parent");
    assert!(refused(&base.join("cache").join("staging"), &dirs).is_none(), "a separate folder is fine");
    let message = refused(&clips, &dirs).unwrap_or_default();
    assert!(message.contains("Clips"), "the message names the folder: {message}");
    let _ = std::fs::remove_dir_all(&base);
}

/// `<worktree>\.tmp\sandbox\<tag>-<uuid>`: tests that stand in for clip folders stay out of %TEMP%.
fn sandbox(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join("sandbox").join(format!("{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_refused_staging_folder_is_neither_used_nor_scanned() {
    let base = sandbox("guarded");
    let clips = base.join("Clips");
    std::fs::create_dir_all(&clips).unwrap();
    let setup = Setup { dir: Some(clips.clone()), recover: Some(clips.clone()), out_dir: clips.clone(), mover: mover::Config::default(), refused: Vec::new() };
    let setup = setup.guarded(std::slice::from_ref(&clips));
    assert!(setup.dir.is_none() && setup.recover.is_none(), "staging off and no recovery scan of the clip folder");
    assert_eq!(setup.refused.len(), 1, "one line says why: {:?}", setup.refused);
    let fine = Setup { dir: Some(base.join("stage")), recover: Some(base.join("stage")), out_dir: clips.clone(), mover: mover::Config::default(), refused: Vec::new() };
    let fine = fine.guarded(std::slice::from_ref(&clips));
    assert!(fine.dir.is_some() && fine.recover.is_some() && fine.refused.is_empty());
    let listed = crate::encoder::staging_guard::parse(br#"{"recordings":"E:\\Clips","imported":["F:\\Old clips"]}"#);
    assert_eq!(listed, vec![PathBuf::from(r"E:\Clips"), PathBuf::from(r"F:\Old clips")], "the app's library folders count too");
    assert!(crate::encoder::staging_guard::parse(b"not json").is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_staging_folder_named_through_a_network_alias_is_still_refused() {
    let base = sandbox("alias");
    let clips = base.join("Clips");
    std::fs::create_dir_all(&clips).unwrap();
    let real = crate::win_volume::plain(&std::fs::canonicalize(&clips).unwrap());
    let text = real.to_string_lossy().into_owned();
    let Some((drive, rest)) = text.split_once(":\\") else { return };
    let alias = PathBuf::from(format!("\\\\localhost\\{}$\\{rest}", drive.to_ascii_lowercase()));
    if !alias.exists() {
        eprintln!("skipped: the admin share is not reachable here");
        return;
    }
    assert!(refused(&alias, std::slice::from_ref(&clips)).is_some(), "one folder under two names");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn recovery_requeues_a_kept_copy_whose_clip_is_only_a_part() {
    // A power cut right after a move: V: lost the `.part` -> `.mp4` rename, C: kept the copy.
    let (stage, out) = (temp_dir("kept-part"), temp_dir("kept-part-out"));
    let kept_dir = stage.join(super::mover::KEPT_DIR);
    std::fs::create_dir_all(&kept_dir).unwrap();
    let name = "A__2026-10-22__09-00-00-000__aa.mp4";
    std::fs::write(kept_dir.join(name), vec![5u8; 1000]).unwrap();
    std::fs::write(out.join(format!("{name}.part")), vec![5u8; 1000]).unwrap();
    // A `.part` of another size is a half copy: that kept copy waits for the app as before.
    let other = "B__2026-10-22__09-01-00-000__bb.mp4";
    std::fs::write(kept_dir.join(other), vec![5u8; 1000]).unwrap();
    std::fs::write(out.join(format!("{other}.part")), vec![5u8; 999]).unwrap();
    // A clip that landed: nothing to recover.
    let landed = "C__2026-10-22__09-02-00-000__cc.mp4";
    std::fs::write(kept_dir.join(landed), vec![5u8; 1000]).unwrap();
    std::fs::write(out.join(landed), vec![5u8; 1000]).unwrap();
    std::fs::write(out.join(format!("{landed}.part")), vec![5u8; 1000]).unwrap();

    assert!(super::super::staging_kept::holds_kept(&stage));
    let done = super::super::staging_kept::hand_back(&stage, &out);
    assert_eq!(done.back, vec![stage.join(name)], "{:?}", done.errors);
    assert!(stage.join(name).is_file() && !kept_dir.join(name).exists(), "handed back to staging");
    // Handed back, it is an ordinary staged clip: the next recovery queues it as well.
    assert_eq!(recover(&stage, &out).moves, vec![(stage.join(name), out.join(name))]);
    assert!(kept_dir.join(other).is_file() && kept_dir.join(landed).is_file(), "the rest stays kept");
    assert!(out.join(format!("{name}.part")).is_file() && out.join(landed).is_file(), "the clip folder is only read");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn only_a_copy_of_a_clip_that_landed_whole_is_deleted_and_leaked_copies_free_the_keep_limit() {
    // RI2-7: the recorder alone decides; a marked copy goes only when its clip is present,
    // readable and the same size. Copies that match no whole clip are set aside, never deleted,
    // and no longer stop new keeps.
    use super::super::staging_kept::{hand_back, LEAKED_DIR};
    let (stage, out) = (temp_dir("kept-check"), temp_dir("kept-check-out"));
    let kept_dir = stage.join(super::mover::KEPT_DIR);
    std::fs::create_dir_all(&kept_dir).unwrap();
    let copy = |name: &str, len: usize| {
        let kept = kept_dir.join(name);
        std::fs::write(&kept, vec![6u8; len]).unwrap();
        std::fs::write(format!("{}:{}", kept.display(), super::mover::KEPT_MARK), b"").unwrap();
        kept
    };
    let whole = copy("W__2026-11-07__10-00-00-000__ww.mp4", 1000);
    std::fs::write(out.join("W__2026-11-07__10-00-00-000__ww.mp4"), vec![6u8; 1000]).unwrap();
    let other_size = copy("S__2026-11-07__10-01-00-000__ss.mp4", 1000);
    std::fs::write(out.join("S__2026-11-07__10-01-00-000__ss.mp4"), vec![6u8; 700]).unwrap();
    let deleted_clip = copy("D__2026-11-07__10-02-00-000__dd.mp4", 1000);
    let done = hand_back(&stage, &out);
    assert!(done.errors.is_empty(), "{:?}", done.errors);
    assert_eq!(done.deleted, 1, "the copy of the clip that landed whole");
    assert!(!whole.exists());
    assert_eq!(done.leaked.len(), 2, "{:?}", done.leaked);
    for kept in [&other_size, &deleted_clip] {
        assert!(!kept.exists() && kept_dir.join(LEAKED_DIR).join(kept.file_name().unwrap()).is_file(), "set aside, not deleted: {}", kept.display());
    }
    // The keep limit counts only kept\ itself: a new 1000-byte keep fits a 1500-byte limit.
    let staged = stage.join("N__2026-11-07__10-03-00-000__nn.mp4");
    std::fs::write(&staged, vec![6u8; 1000]).unwrap();
    assert!(super::mover::keep_staged_within(&staged, &out.join("N__2026-11-07__10-03-00-000__nn.mp4"), 1500).is_some());
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}
